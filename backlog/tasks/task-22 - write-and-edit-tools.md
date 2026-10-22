---
id: TASK-22
title: write and edit tools
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:11'
labels:
  - tools
  - problem-1
  - problem-3
milestone: m-2
dependencies:
  - TASK-21
priority: high
type: feature
ordinal: 22000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Edits are where silent corruption happens: editing a stale view, ambiguous matches, broken line endings.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 edit does exact string replacement and fails if the match is not unique unless replace_all is set
- [x] #2 edit and overwrite of an existing file fail if the file changed since it was last read
- [x] #3 Writes are atomic and preserve permissions and line-ending style
- [x] #4 Each mutation emits a file-change event with a diff for the change ledger
- [x] #5 Errors tell the model exactly how to recover
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
1. Shared Mutator in write.rs: fresh_content (NotRead/StaleRead via ReadTracker) and commit (replace_atomic, record new hash, unified diff, FileChanged event). 2. write tool: create or overwrite, keep CRLF. 3. edit tool: exact replace, unique unless replace_all, CRLF-aware, recovery hints (line-number prefixes, whitespace-only near-match line, ambiguous lines). 4. Tests per AC.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Validation: cargo test -p harness-tools (50 unit + 1 doctest pass), cargo clippy -p harness-tools --all-targets -- -D warnings clean, cargo doc warning-free. File-change events are harness_tools::tool::ToolEvent::FileChanged(FileChange{tool,path,kind,before_hash,after_hash,diff}) emitted via ToolContext's ToolEventSink; agent maps them to the run store / change ledger. DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
harness_tools::write::WriteTool and harness_tools::edit::EditTool: stale-read protected (must read first; fails if hash changed), atomic writes keeping permissions and CRLF, FileChanged events with unified diff, model-facing errors with recovery steps. Verified by write::tests and edit::tests.
<!-- SECTION:FINAL_SUMMARY:END -->
