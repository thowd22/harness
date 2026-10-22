---
id: TASK-2
title: INI config loader for harness.ini
status: Done
assignee:
  - '@config-ini'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:19'
labels:
  - config
  - core
  - problem-9
milestone: m-0
dependencies:
  - TASK-1
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 2000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The user wants one simple INI file to hold all harness configuration including credentials and keybinds. Typos in config silently changing behaviour is a known fragility source (problem 9), so parsing must be strict and errors precise.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Loads from HARNESS_CONFIG or XDG config path, with built-in defaults when absent
- [x] #2 Supports all sections in the harness.ini reference including dotted subsections like provider.anthropic and remote.name
- [x] #3 Unknown sections or keys produce errors with file and line number
- [x] #4 Typed accessors for strings, bools, numbers, comma lists, durations and paths with tilde expansion
- [x] #5 harness config check validates the file and exits non-zero on error
- [x] #6 Unit tests cover defaults, overrides, list parsing and error locations
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
1. Hand-written strict INI lexer in ini::parse (line numbers, duplicate detection) since rust-ini drops positions.
2. Typed accessors on ini::Section/Entry (str, bool, numbers, lists, durations, tilde paths), each error carrying file:line.
3. Schema module: known sections/keys incl. provider.<id> and remote.<name>; unknown section/key -> error with location.
4. ini::Config typed struct with defaults; Config::load() resolving HARNESS_CONFIG / XDG path; missing file -> defaults.
5. ini::check(path) entry point for 'harness config check' (CLI wiring lives in the harness binary).
6. Unit tests + docs/config.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in crates/harness-config/src/ini.rs + ini/{document,error,value,config,load}.rs. Own strict lexer (rust-ini loses line numbers). Config::from_document is the single schema source: SectionReader tracks consumed keys and leftover keys become UnknownKey errors with file:line and a did-you-mean hint. All errors collected and sorted by line. Fixture tests/fixtures/reference.ini is the doc-2 example and parses fully. Docs: docs/config.md, doc-2 appended with value syntax/defaults. DoD#4 N/A (no TUI).
REMAINING (AC#5, DoD#1): the 'harness config check' subcommand must be wired in crates/harness/src/main.rs (not owned by config-ini): call harness_config::ini::run_check(path_opt, &mut stdout, &mut stderr) and std::process::exit with its return code (0 ok / 1 errors), plus a binary-level test. The library function and its exit codes are unit-tested (ini::load::tests::check_reports_ok_and_errors_with_exit_codes). Verified: cargo test -p harness-config (42 unit + 5 integration + 1 doctest pass), cargo clippy -p harness-config --all-targets -- -D warnings clean, RUSTDOCFLAGS=-D warnings cargo doc clean.

Integration: `harness config check [PATH]` is wired in crates/harness/src/main.rs and covered by crates/harness/tests/headless.rs::config_check_and_schema_subcommands (exit 0 on a valid file, 1 on an unknown section). Workspace fmt/clippy/test green.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
INI loader with strict located errors and typed accessors; `harness config check` wired in the binary. Verified by harness-config unit tests and the binary test config_check_and_schema_subcommands.
<!-- SECTION:FINAL_SUMMARY:END -->
