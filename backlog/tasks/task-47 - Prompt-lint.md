---
id: TASK-47
title: Prompt lint
status: Done
assignee:
  - '@yaml-config'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - prompts
  - problem-2
milestone: m-4
dependencies:
  - TASK-43
  - TASK-44
  - TASK-46
priority: high
type: feature
ordinal: 47000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Over-literal reading of boilerplate prohibitions such as DO NOT MODIFY tests is the largest failure cluster in published evaluations (problem 2). The harness should catch these patterns before they ship.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Flags blanket prohibitions over broad categories such as tests or config and suggests positive-scope rewording
- [x] #2 Flags contradictions between prompt layers, agent config, skills and memory
- [x] #3 Flags prompts over a token budget
- [x] #4 harness lint exits non-zero on errors and warnings appear in doctor
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
1. lint::Finding{severity,rule,location,message,suggestion}.
2. Rules: blanket prohibition over broad categories (with positive-scope suggestion), cross-source contradictions (prompt layers, agent config tools, skills, memory text), token budget.
3. lint_catalog + LintReport (errors/warnings), run_lint(...) -> exit code for harness lint; warnings() for doctor.
4. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_config::lint: Severity, Rule (blanket-prohibition, contradiction, token-budget, invalid), Loc, Finding, LintSource (+ memory()), LintOptions{prompt_token_budget=4000}, estimate_tokens, blanket_prohibitions, contradictions (forbid vs require/permit directives across prompt layers, prompt_text, model-invocable skills, memory), tool_contradictions (prompt vs agent tools), lint_catalog(catalog, Option<&ValidationContext>, memory, opts) -> LintReport{errors(),warnings(),has_errors(),summary()}, run_lint(report, out) -> 1 on errors else 0, doctor_lines(report). Memory findings are warnings. No new ini keys (budget is a LintOptions field).
Remaining: AC#4 needs (a) the binary owner to add `harness lint` in crates/harness/src/main.rs: load ini, Roots::resolve, Catalog::load, lint_catalog(.., Some(&ValidationContext::from_config(&cfg)), &memory_sources, &LintOptions::default()), exit with run_lint(&report, &mut stdout), plus a binary test; (b) TASK-113 doctor to print lint::doctor_lines(&report) and Catalog::shadowed(). Library exit codes and doctor lines are tested. cargo test -p harness-config: 88 unit + 6 integration tests pass; cargo clippy -p harness-config --all-targets -D warnings clean; cargo fmt check clean.

Integration: `harness lint` in the binary (exit 1 on errors) and lint findings in `harness doctor`. Tests: crates/harness/tests/commands.rs lint_exits_nonzero_on_errors, doctor_checks_every_area_without_the_network.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: `harness lint` in the binary (exit 1 on errors) and lint findings in `harness doctor`. Tests: crates/harness/tests/commands.rs lint_exits_nonzero_on_errors, doctor_checks_every_area_without_the_network. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
