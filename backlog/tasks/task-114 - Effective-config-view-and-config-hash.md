---
id: TASK-114
title: Effective config view and config hash
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - config
  - problem-9
milestone: m-11
dependencies:
  - TASK-2
  - TASK-41
priority: medium
type: feature
ordinal: 114000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Knowing exactly which configuration produced a run is required to diagnose fragility.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 /config and harness config show print effective, redacted config with source of each value
- [x] #2 A stable hash of effective config and loaded YAML is stamped on every run
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
harness_config::effective: rows() (value + file:line source, secrets redacted), render() with loaded definitions, config_hash()/hash_input() over effective config (minus machine paths) + active YAML, run_show() for harness config show.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: harness_config::effective::{Row, rows, render, config_hash, hash_input, run_show, DEFAULT_SOURCE} (new module; one pub mod line added to harness-config lib.rs). Tests: effective::tests (sources and redaction, hash stable across file locations and data_dir, sensitive to values and YAML, run_show exit codes). Eval results are stamped with it (Results.config_hash). Docs: docs/effective-config.md.
Remaining (AC#1, AC#2, DoD#1): (a) binary: 'harness config show [path]' -> effective::run_show; TUI: /config renders effective::render(&cfg, Some(&catalog)); (b) harness-events: add config_hash (e.g. to RunHeader or RunStarted) and the binary/agent fills it with config_hash(&cfg, Some(&catalog)) on every run; tests for both. Then check AC#1, AC#2, DoD#1.

Integration: `harness config show` and /config print effective redacted config with sources; RunHeader.config_hash stamped on every run (RecorderOptions::with_config_hash). Tests: commands.rs config_show_prints_sources_and_redacts, every_run_is_stamped_with_the_config_hash.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: `harness config show` and /config print effective redacted config with sources; RunHeader.config_hash stamped on every run (RecorderOptions::with_config_hash). Tests: commands.rs config_show_prints_sources_and_redacts, every_run_is_stamped_with_the_config_hash. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
