---
id: TASK-7
title: Data directory layout and migrations
status: Done
assignee:
  - '@config-ini'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:09'
labels:
  - core
  - infra
milestone: m-0
dependencies:
  - TASK-2
priority: medium
type: task
ordinal: 7000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Runs, sessions, trust store, memory drive, checkpoints and caches all live under data_dir. A defined layout with a version marker avoids ad-hoc paths and lets future versions migrate safely.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Documented layout for runs, sessions, checkpoints, trust, memory, remote cache and logs
- [x] #2 A layout version file exists and an upgrade path is exercised by a test
- [x] #3 Internal state directories are registered as protected paths
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
1. paths: config file lookup (HARNESS_CONFIG / XDG), default data_dir (XDG_DATA_HOME / ~/.local/share/harness).
2. DataDir with runs, sessions, checkpoints, trust, memory, remote-cache, logs.
3. layout-version file + Migration chain; v0 (unversioned, logs-only) -> v1.
4. DataDir::protected_paths for the policy engine.
5. docs/data-dir.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
crates/harness-config/src/paths.rs. DataDir::open() creates dirs (0700) and runs paths::migrate(root, MIGRATIONS, LAYOUT_VERSION); version file rewritten atomically after each step; newer version -> LayoutError::TooNew. Tests: upgrade of unversioned dir keeps old logs, synthetic 3-version chain, TooNew/NoMigration, bad version file. AC#3: DataDir::protected_paths(config_file) returns data_dir, every internal dir (incl. external memory drive), version file and harness.ini; harness-policy (TASK-25) must add these to its protected set (cross-crate). DoD#4 N/A. Verified: cargo test -p harness-config (42 unit + 5 integration + 1 doctest pass), cargo clippy -p harness-config --all-targets -- -D warnings clean, RUSTDOCFLAGS=-D warnings cargo doc clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Versioned data_dir layout with migration chain and protected-path list; verified by paths::tests.
<!-- SECTION:FINAL_SUMMARY:END -->
