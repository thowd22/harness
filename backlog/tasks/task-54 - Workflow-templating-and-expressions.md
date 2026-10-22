---
id: TASK-54
title: Workflow templating and expressions
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:14'
labels:
  - workflow
milestone: m-5
dependencies:
  - TASK-53
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 54000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Steps pass data to each other through templated prompts and conditions.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 minijinja over inputs, steps outputs, status and artifacts, matrix and run
- [x] #2 No access to environment variables or secrets
- [x] #3 References are validated statically at load
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
1. harness_workflow::template: one strict minijinja Environment (no loader, debug() removed) with an explicit context: inputs, steps.<id>.outputs/status/artifacts, matrix, item, run.
2. render / eval / eval_bool / render_value (typed {{ expr }}).
3. Static checks: references() + check_refs() against RefScope (declared inputs, upstream steps, declared outputs/artifacts, matrix keys), called from schema validation.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-workflow/src/template.rs (+ template/tests.rs). Nothing outside the context exists: no environment, file or secret access, undefined variables are errors, and the debug() global is removed. References are checked at load: unknown inputs, unknown or non-upstream steps, undeclared outputs/artifacts, matrix/item outside expansions, unknown run fields and unknown roots (with a message that env vars and secrets are unavailable). Steps re-run by a loop may read the looping step. Validation: cargo test -p harness-workflow (36 unit + 2 bundled + 12 executor integration tests pass); cargo clippy -p harness-workflow -p harness-agent --all-targets -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_workflow::template (render, eval, eval_bool, render_value, references, check_refs, Context/StepView/RunView). minijinja runs over inputs, step outputs/status/artifacts, matrix and run, with no env or secret access, and references are validated statically. Verified by 5 template tests and the schema reference tests. DoD#4 N/A.
<!-- SECTION:FINAL_SUMMARY:END -->
