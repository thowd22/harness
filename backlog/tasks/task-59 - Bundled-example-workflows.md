---
id: TASK-59
title: Bundled example workflows
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - workflow
  - problem-5
milestone: m-5
dependencies:
  - TASK-119
  - TASK-55
  - TASK-79
  - TASK-80
priority: medium
type: feature
ordinal: 59000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Good defaults show users how to build verification loops and encode the research findings.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Ships implement-verify-review loop, fix-bug with fail-before and pass-after proof, and plan-approve-implement workflows
- [x] #2 Each bundled workflow has an eval task
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
1. crates/harness-workflow/workflows/{implement-verify-review,fix-bug,plan-approve-implement}.yaml using the built-in coder/researcher/reviewer agents; exposed as harness_workflow::bundled::BUNDLED (BuiltinFile) and with_builtins(BUILTINS).
2. Eval suite crates/harness-workflow/evals/workflows/suite.yaml (harness-eval format) with one task per workflow, a calc fixture and fake scripts.
3. Tests: validate against built-in agents, discovered with builtins, eval suite parses, and each eval task runs in-process with real tools, checkpoints and the fake script, then its checks pass.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Workflows, eval suite and tests are done (bundled.rs unit tests, tests/bundled.rs in-process eval runs, fix-bug's proof proven). Remaining to actually ship them in the binary: the binary or harness-config builtins owner must pass harness_workflow::bundled::with_builtins(harness_config::builtins::BUILTINS) to discovery::Roots::resolve instead of BUILTINS, and record a Defaults entry in CHANGELOG.md per docs/defaults.md (the builtins fingerprint changes). For harness-eval: workflow tasks with approval gates need gates approved up front (e.g. harness wf run --approve-all) once headless workflow runs exist. Then check AC#1 and DoD#1. AC#2 (an eval task per bundled workflow) is checked. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.

Integration: bundled workflows (implement-verify-review, fix-bug, plan-approve-implement) ship via harness-config assets/workflows and BUILTINS; CHANGELOG Defaults entry (fingerprint c0b3a2ab142f). Test: commands.rs wf_lists_describes_and_runs_bundled_workflows; harness-workflow bundled test.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: bundled workflows (implement-verify-review, fix-bug, plan-approve-implement) ship via harness-config assets/workflows and BUILTINS; CHANGELOG Defaults entry (fingerprint c0b3a2ab142f). Test: commands.rs wf_lists_describes_and_runs_bundled_workflows; harness-workflow bundled test. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
