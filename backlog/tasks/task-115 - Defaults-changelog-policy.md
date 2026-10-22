---
id: TASK-115
title: Defaults changelog policy
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:05'
labels:
  - docs
  - problem-9
milestone: m-11
dependencies:
  - TASK-111
priority: low
type: docs
ordinal: 115000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Default changes like reasoning effort or verbosity caused real regressions. They should be deliberate and documented.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 CHANGELOG has a Defaults section and CI requires an entry when built-in prompts or defaults change
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
CHANGELOG.md with a ## Defaults section; harness_eval::defaults fingerprint (built-in files + harness.ini defaults) and check_changelog; tests/defaults_changelog.rs runs in cargo test --workspace (CI).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
CI already runs cargo test --workspace, so tests/defaults_changelog.rs gates every default change: it fails with the exact line to add (defaults-fingerprint: <12 hex>) until the Defaults section names the current fingerprint. Unit tests cover section detection and stale entries. Docs: docs/defaults.md#changing-a-default. Note for other engineers: changing Config::defaults() or any built-in asset requires a CHANGELOG entry.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added CHANGELOG.md Defaults section and a CI-enforced fingerprint check (harness_eval::defaults, tests/defaults_changelog.rs).
<!-- SECTION:FINAL_SUMMARY:END -->
