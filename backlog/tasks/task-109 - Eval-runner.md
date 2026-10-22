---
id: TASK-109
title: Eval runner
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - eval
  - problem-6
  - problem-9
milestone: m-11
dependencies:
  - TASK-108
  - TASK-18
priority: high
type: feature
ordinal: 109000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Measure success, cost and hygiene together, since harness choice often affects cost more than success rate.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 harness eval run executes tasks headless in isolated temp workspaces, in parallel
- [x] #2 Records pass rate, cost, tokens, wall time, hygiene violations, approvals requested and premature stops
- [x] #3 Writes JSON results stamped with config hash and harness version
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
1. runner::workspace: fresh temp workspace per attempt (fixture copy, files, setup).
2. runner::exec: Executor trait; ProcessExecutor spawns harness run --json; InProcessExecutor runs the agent loop with the fake provider.
3. runner::checks + metrics_from_events; run_suite with a semaphore; Results JSON stamped with harness_version and config_hash.
4. cli::main for harness eval run/compare.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: harness_eval::runner::{run_suite, RunOptions, Stamp, Results, TaskResult, AttemptResult, Summary, EventMetrics, Tokens, metrics_from_events, stop_code}, runner::exec::{Executor, Execution, ProcessExecutor, InProcessExecutor, parse_events}, runner::workspace, runner::checks, harness_eval::cli::{main, Context}. Tests: runner tests (parallel, isolated workspaces, filters, JSON round trip with stamp), exec tests (args, stub binary, timeout kills the process group), tests/starter.rs (in-process, and an ignored test through the real binary that passed 5/5 runs locally with HARNESS_BIN=target/debug/harness; one earlier run failed with exit 1 and no events while the binary was being rebuilt by another engineer, not reproducible).
Remaining (AC#1, DoD#1): the binary must route 'harness eval ...' to harness_eval::cli::main(&args_after_eval, &Context{harness_bin: current_exe, harness_version: env!(CARGO_PKG_VERSION), config_hash: harness_config::effective::config_hash(&cfg, Some(&catalog))}, stdout, stderr) and exit with its code; add a binary test. Then check AC#1 and DoD#1. Note: process attempts share the user's data dir (sessions/runs land there).

Integration: `harness eval` routes to harness_eval::cli::main; process attempts run headless in temp workspaces with --parallel (default 4). Tests: commands.rs eval_runs_a_suite_through_the_binary, long_task_evals_pass_through_the_binary.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: `harness eval` routes to harness_eval::cli::main; process attempts run headless in temp workspaces with --parallel (default 4). Tests: commands.rs eval_runs_a_suite_through_the_binary, long_task_evals_pass_through_the_binary. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
