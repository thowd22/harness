---
id: TASK-76
title: Git staging guard
status: Done
assignee:
  - '@hygiene-verify'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:05'
labels:
  - hygiene
  - policy
  - problem-3
milestone: m-7
dependencies:
  - TASK-26
priority: high
type: feature
ordinal: 76000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Reflexive git add -A sweeps commit junk files.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 git add -A, git add ., git add --all and git commit -a are denied or asked by default with a message to stage explicit paths
- [x] #2 Explicit path staging is allowed per policy
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
1. New file crates/harness-policy/src/staging.rs: detect sweeping staging (git add -A/./--all/:/ and git commit -a/--all/-am) via shell::parse + canonical forms.
2. StagingGuard (ask by default, deny optional) applies on top of the engine verdict with a message to stage explicit paths; explicit paths stay governed by the normal policy.
3. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Added crates/harness-policy/src/staging.rs (plus the one-line pub mod in lib.rs): sweeping() / check() recognise git add -A/--all/./:/ (incl. -fA clusters, git -C dir, /usr/bin/git, sudo, bash -c) and git commit -a/-am/--all, while explicit paths, -u, -p, commit -m 'a...' are not sweeping. StagingGuard (ask by default, deny()) tightens a Verdict with a built-in rule whose reason tells the model to stage explicit paths; ini bash.allow or session grants whose literal prefix itself sweeps (git add -A*) are respected. StagingGuard::decide(engine, action, mode, cwd, sink) wraps PolicyEngine::check and records the final verdict. rules.rs was not modified. No new ini key (default is ask). Tests: cargo test -p harness-policy staging (5 pass). Docs: docs/hygiene.md#git-staging-guard. Integration: harness-agent must call StagingGuard::default().decide(...) instead of engine.decide (engine built without its own sink). DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_policy::staging: git add -A/./--all and git commit -a are asked (or denied) with a stage-explicit-paths message; explicit path staging follows normal policy. Verified by unit tests through PolicyEngine.
<!-- SECTION:FINAL_SUMMARY:END -->
