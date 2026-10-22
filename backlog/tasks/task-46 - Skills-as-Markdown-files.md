---
id: TASK-46
title: Skills as Markdown files
status: Done
assignee:
  - '@yaml-config'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:32'
labels:
  - skills
  - commands
  - problem-6
milestone: m-4
dependencies:
  - TASK-45
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 46000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Skills should be simple Markdown files callable as slash commands, and optionally discoverable by the model at minimal token cost.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Loads skills from skills dir as name.md or name/SKILL.md, frontmatter optional
- [x] #2 Without frontmatter, name is the file stem and description is the first non-heading line
- [x] #3 /name args injects the body with $ARGUMENTS and positional substitution
- [x] #4 model_invocable skills are listed as one line each with path, loaded by the model via read
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
1. skills::Skill parse with optional YAML frontmatter; name.md and name/SKILL.md.
2. Defaults: file stem / first non-heading line.
3. expand(args) with $ARGUMENTS and $1..$9 (quote-aware split).
4. listing() of model_invocable skills, one line each with path.
5. Hook into discovery Catalog; tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_config::skills: Skill (Artifact) from skills/name.md or skills/name/SKILL.md, optional frontmatter (name, description, args, model_invocable) with located errors; defaults from file stem / dir name and first non-heading line. Skill::expand handles $ARGUMENTS, $1..$9 (split_args, shell-style quotes), $$; args appended if no placeholder. listing() gives one line per model_invocable skill with path (built-ins without a disk path omitted). resolve_command supports /name, /global:name, /project:name. Injecting the expanded text as a user turn is the slash-command/agent owner's job (TASK-45). cargo test -p harness-config: 88 unit + 6 integration tests pass; cargo clippy -p harness-config --all-targets -D warnings clean; cargo fmt check clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Markdown skills with optional frontmatter, stem/first-line defaults, argument substitution and a compact model listing with paths. Verified by skills.rs and discovery tests; documented in docs/yaml-config.md.
<!-- SECTION:FINAL_SUMMARY:END -->
