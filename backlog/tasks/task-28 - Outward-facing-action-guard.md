---
id: TASK-28
title: Outward-facing action guard
status: Done
assignee:
  - '@policy'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:47'
labels:
  - policy
  - security
  - problem-7
  - problem-8
milestone: m-2
dependencies:
  - TASK-26
  - TASK-27
priority: high
type: feature
ordinal: 28000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents pushing, publishing or opening PRs without permission is a top trust complaint. Outward actions must ask regardless of autonomy mode.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Default outward rules cover git push, PR creation, package publish and similar commands, configurable in outward.ask
- [x] #2 Outward rules ask even in auto mode unless explicitly allowed in ini
- [x] #3 Tests cover chained and aliased forms
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
1. outward.rs: DEFAULT_OUTWARD patterns + canonical_forms (strip program path, git/gh/npm/cargo/docker global options, sub-command aliases).
2. PolicyEngine: outward step after deny, asks in every mode; explicit ini bash.allow (fixed prefix is itself outward) allows; session grants allow.
3. Tests for chained/wrapped/aliased forms; docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented in harness_policy::outward and rules::PolicyEngine::decide_command step 3. outward.ask from ini and YAML overlays adds to DEFAULT_OUTWARD (git push, send-email, gh pr create/merge/review/comment, gh issue/release/repo/gist create, gh api, glab mr, cargo publish/yank/owner, npm/yarn/pnpm/bun publish, twine, poetry, uv, flit, hatch, gem push, docker/podman/buildah/helm push, dotnet nuget push, mvn deploy, gradle publish, terraform apply, kubectl apply). 'Explicitly allowed in ini' = a [policy] bash.allow pattern whose fixed prefix (text before the first wildcard) itself matches the outward pattern, e.g. 'git push origin feature/*'; broad patterns like 'git *' or '*' do not bypass; deny still wins; dynamic commands never auto-allowed. No new ini keys needed. Tests: rules::tests outward_actions_ask_even_in_auto, outward_chained_and_aliased_forms (25 forms), outward_explicit_ini_allow_and_precedence; outward::tests (3).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Outward guard: default + configurable outward.ask patterns matched against canonical forms of every parsed simple command; asks in all modes unless an explicit ini bash.allow names the command. DoD#4 N/A (no UI). Verified with cargo test -p harness-policy and clippy -D warnings.
<!-- SECTION:FINAL_SUMMARY:END -->
