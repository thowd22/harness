---
id: TASK-6
title: 'Tracing, log files and terminal-safe panic handling'
status: Done
assignee:
  - '@scaffold'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:19'
labels:
  - core
  - infra
milestone: m-0
dependencies:
  - TASK-1
priority: medium
type: feature
ordinal: 6000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
A TUI that panics in raw mode leaves the user's terminal broken, and debugging harness issues requires structured logs that do not pollute the UI.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 tracing logs go to a rotating file in data_dir with level from HARNESS_LOG
- [x] #2 Panic hook restores the terminal before printing the panic and writes it to the log
- [x] #3 Logs pass through the redaction layer
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
1. harness_events::logging: init/build subscriber writing to daily-rotating file in <data_dir>/logs, filter from HARNESS_LOG (default info), all output via a redacting MakeWriter using a LineRedactor trait (TASK-5 implements it on its redactor).
2. harness_tui::terminal: restore_terminal() and install_panic_hook() wrapping previous hook: restore terminal -> tracing::error! panic (redacted log) -> previous hook prints.
3. Wire both in harness bin main.
4. Tests: rotation file location + level filtering + redaction; panic hook ordering (restore before print, panic logged).
5. docs/logging.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_events::logging (LogConfig, build_subscriber, init, resolve_filter, log_dir, LineRedactor trait, NoRedaction, RedactingMakeWriter/RedactingWriter): daily-rotating <data_dir>/logs/harness.YYYY-MM-DD.log, keeps 7, no ANSI, filter from HARNESS_LOG (default info), invalid filter -> LogError::Filter (binary prints warning and continues). Every formatted event is buffered and passed through a LineRedactor before hitting disk. harness_tui::terminal: restore_terminal() (no-op unless raw mode), install_panic_hook()/install_panic_hook_with(restore): restore -> tracing::error!(target harness::panic) -> previous hook. Wired in crates/harness/src/main.rs. Tests: 6 unit tests in logging (rotation file location/name, level filter, redaction, invalid filter, buffering), terminal unit test, integration tests crates/harness-tui/tests/panic_hook_order.rs (restore, log, print order) and panic_hook_log.rs (panic lands in redacted log file). Docs: docs/logging.md. REMAINING for AC#3: the real redaction layer (TASK-5, harness_events::redact) does not exist yet; main currently uses NoRedaction. TASK-5 must impl harness_events::logging::LineRedactor for its redactor and pass it via LogConfig::with_redactor in crates/harness/src/main.rs, plus a test using it; then check AC#3/DoD#1 and set Done. DoD#4 N/A (nothing visible in TUI).

Integration: main.rs startup() passes the credential Redactor via LogConfig::with_redactor. Test crates/harness/tests/headless.rs::log_file_is_redacted_with_resolved_credentials (secret absent from data/logs, [REDACTED] present).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Rotating redacted log files and terminal-safe panic hook, wired in the binary. Verified by logging unit tests, panic hook integration tests and log_file_is_redacted_with_resolved_credentials.
<!-- SECTION:FINAL_SUMMARY:END -->
