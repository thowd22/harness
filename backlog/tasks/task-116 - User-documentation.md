---
id: TASK-116
title: User documentation
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:05'
labels:
  - docs
milestone: m-11
dependencies:
  - TASK-113
priority: medium
type: docs
ordinal: 116000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need a concise guide to config, YAML, skills, workflows, memory and remotes.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Docs cover install, harness.ini, YAML spec, skills, workflows, memory, remotes and the palette
- [x] #2 Every example in docs is validated by a test
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
docs/guide/ (install, harness.ini, YAML, skills, workflows, memory, remotes, palette) plus area docs; tests/docs_examples.rs validates every fenced example by tag (ini, yaml prompt/agent/workflow, markdown skill, yaml eval-suite, sh).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Guide: docs/guide/README.md and 8 pages. Validation: crates/harness-eval/tests/docs_examples.rs loads every ini block, loads YAML/skill blocks in a global scope over the built-ins (no load errors, agent validation, no prompt-lint errors), parses eval suites, checks harness commands in sh blocks, and fails on untagged fences; it caught two invalid examples while writing. Also validates docs/defaults.md, evals.md, dreaming.md, memory-ui.md, golden-snapshots.md, effective-config.md, doctor.md. Commands documented but not yet wired in the binary: harness config show, doctor, eval (see TASK-109/113/114).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
User guide under docs/guide covering install, harness.ini, YAML, skills, workflows, memory, remotes and the palette; every example validated by tests/docs_examples.rs.
<!-- SECTION:FINAL_SUMMARY:END -->
