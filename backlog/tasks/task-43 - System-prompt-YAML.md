---
id: TASK-43
title: System prompt YAML
status: Done
assignee:
  - '@yaml-config'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:32'
labels:
  - config
  - prompts
  - problem-2
  - problem-6
milestone: m-4
dependencies:
  - TASK-41
  - TASK-42
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 43000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users want system prompts configurable in YAML. Prompt order must be stable so provider caching works.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Prompt files support ordered layers from inline text or Markdown files
- [x] #2 Template variables are limited to an allowlist such as cwd, os, date and git branch, never env or secrets
- [x] #3 Assembled prompt is byte-identical across turns of a session
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
1. prompts::Prompt{name,description,layers,vars}; layers text|file (relative, no escape).
2. Template vars allowlist (cwd, os, date, git_branch); minijinja strict undefined; undeclared vars rejected at load.
3. SessionPrompt renders once per session from a frozen PromptVars snapshot so text is byte-identical across turns.
4. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_config::prompts: Prompt (Artifact), Layer/LayerSource, ALLOWED_VARS=[cwd, os, date, git_branch], PromptVars::detect (date UTC, git branch from .git/HEAD, no subprocess), Prompt::render, SessionPrompt (rendered once, sha256). minijinja Environment::empty + strict undefined: no filters/functions/globals; undeclared vars and non-allowlisted vars rejected at load with located errors. File layers relative to the yaml, no absolute/.., no symlinks in project scope. cargo test -p harness-config: 88 unit + 6 integration tests pass; cargo clippy -p harness-config --all-targets -D warnings clean; cargo fmt check clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Prompt YAML with ordered text/file layers, allowlisted template vars with no env/secret reach, and a SessionPrompt that is byte-identical across turns. Verified by unit tests in prompts.rs; documented in docs/yaml-config.md.
<!-- SECTION:FINAL_SUMMARY:END -->
