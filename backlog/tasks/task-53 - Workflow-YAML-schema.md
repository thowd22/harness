---
id: TASK-53
title: Workflow YAML schema
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:14'
labels:
  - workflow
  - config
milestone: m-5
dependencies:
  - TASK-42
  - TASK-44
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 53000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agent workflows are a core feature. A GitHub-Actions-like schema maps naturally onto the map visualization.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Supports inputs, steps, needs, if, for_each or matrix, loop with until and max_iterations, retries, timeout, on_failure, runs_on, scope, budget, outputs and artifacts
- [x] #2 Step types: agent, shell, verify, bugfix-proof, approval and nested workflow
- [x] #3 Cycles, unknown references and invalid step types are errors at load
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
1. harness_workflow::schema: serde docs with deny_unknown_fields parsed through harness_config::yaml (file:line:col), typed Workflow/Step/StepKind.
2. Validate: ids, needs, cycles (with path), step type/key fit, loop.retry_step upstream, tasks->steps, template refs; CheckContext for agents/hosts/nested workflows.
3. Tests for every key and every error class; docs/workflows.md and doc-3 decisions.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-workflow/src/schema.rs (+ schema/tests.rs). Every doc-3 key is supported: inputs, steps, needs, if, for_each, matrix, loop {until, max_iterations, retry_step}, retries, timeout, on_failure, runs_on, scope, budget, outputs, artifacts, goal, tasks. Step types: agent, shell, verify, bugfix-proof, approval, workflow; type is inferred from the key. Load errors: unknown keys, invalid type, keys foreign to the type, duplicate/bad ids, unknown needs, cycles with path, retry_step not upstream, tasks to unknown steps, unresolved template refs; with CheckContext also unknown agents, hosts and workflows; check_uses_cycles covers cross-workflow uses cycles. Added keys (recorded in doc-3): budget.warn_at, budget.on_exceed, runs_on min_cpus/min_gpus/fallback, bugfix-proof keep. Follow-up for yaml-config: harness_config::yaml::schema workflow_schema needs these keys so editor completion matches. Validation: cargo test -p harness-workflow (36 unit + 2 bundled + 12 executor integration tests pass); cargo clippy -p harness-workflow -p harness-agent --all-targets -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_workflow::schema (parse, parse_with, from_entry, CheckContext, Workflow/Step/StepKind and friends). All spec keys and step types are supported, and cycles, unknown references and invalid step types are located load errors. Verified by 9 schema tests plus the bundled-workflow validation tests. DoD#4 N/A.
<!-- SECTION:FINAL_SUMMARY:END -->
