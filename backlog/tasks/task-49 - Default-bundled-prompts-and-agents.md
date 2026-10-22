---
id: TASK-49
title: Default bundled prompts and agents
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - prompts
  - problem-2
  - problem-3
  - problem-5
milestone: m-4
dependencies:
  - TASK-43
  - TASK-44
priority: high
type: feature
ordinal: 49000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Defaults determine most users' experience. They must be short, use positive scope, and encode the known fixes: verify before claiming done, no reflexive broad staging, minimal changes, clean up temporary files.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Built-in default coder, researcher and reviewer agents and prompts ship in the binary and are overridable
- [x] #2 Default prompts pass prompt lint and fit the initial context budget
- [x] #3 Default prompts are covered by golden snapshot tests
- [x] #4 Default coder agent includes the todo tool and its prompt explains when to use it for multi-step work
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
1. Add assets under crates/harness-config/assets (prompts coder/researcher/reviewer + shared memory/todo layers, agents coder/researcher/reviewer, workflows/dream.yaml).
2. New module harness_config::builtins exposing BUILTINS: &[BuiltinFile] via include_str!, plus roots helper.
3. Tests: catalog loads with no errors, lint clean with ValidationContext, token budget, golden snapshots (insta) of rendered prompts, override by global scope, coder has todo + prompt explains todo, memory section == harness_memory PROMPT_SECTION (test in harness-memory).
4. docs/defaults.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_config::builtins (new module + assets under crates/harness-config/assets): BUILTINS (prompts base.md/coder.md/memory.md/coder.yaml/researcher.yaml/reviewer.yaml, agents coder/researcher/reviewer, workflows/dream.yaml), AGENTS, DEFAULT_AGENT, DREAM_WORKFLOW, MEMORY_LAYER, roots(), file(), fingerprint(). Prompts use positive scope and encode verify-before-done, stage-by-name, minimal changes, temp-file cleanup; coder has todo and explains when to use it.
Tests: builtins::tests (load without errors, lint clean with default ValidationContext, under a quarter of the 4000-token budget, todo + explanation, known fixes present, memory layer for read-write agents, override by global scope, fingerprint) and golden snapshots of every rendered prompt in crates/harness-eval/tests/golden_requests.rs (prompt_coder/researcher/reviewer). Docs: docs/defaults.md.
Remaining (AC#1, DoD#1): the binary still calls Roots::resolve(&[], ...) in crates/harness/src/headless.rs and falls back to wiring::default_agent(). The binary owner must pass harness_config::builtins::BUILTINS there (and drop or keep the fallback), and add a binary test that the default coder comes from the built-ins. Then check AC#1 and DoD#1.

Integration: the binary resolves catalogs with bundled::with_builtins(BUILTINS); the default agent is the built-in coder (no fallback). Test: commands.rs default_agent_is_the_builtin_coder_with_todo. CHANGELOG Defaults entry recorded.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: the binary resolves catalogs with bundled::with_builtins(BUILTINS); the default agent is the built-in coder (no fallback). Test: commands.rs default_agent_is_the_builtin_coder_with_todo. CHANGELOG Defaults entry recorded. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
