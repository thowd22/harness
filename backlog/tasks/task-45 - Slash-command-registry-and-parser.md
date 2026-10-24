---
id: TASK-45
title: Slash command registry and parser
status: Done
assignee:
  - '@context-cost-commands'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-24 01:15'
labels:
  - commands
  - tui
milestone: m-4
dependencies:
  - TASK-41
  - TASK-35
  - TASK-36
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 45000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Workflows, skills, agents and built-ins must all be callable via slash commands, from one registry shared by input completion and the palette.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Registry holds built-ins, workflows, skills and agent switches with name, description and argument spec
- [x] #2 Collisions keep both reachable as global:name and project:name
- [x] #3 Arguments parse positional, key=value and quoted values with helpful errors
- [x] #4 Registry updates on hot reload
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
commands::{parse, registry, builtin}: CommandSpec{name, description, kind, scope, args: ArgSpec, strict}; CommandRegistry builds built-ins + catalog workflows (inputs -> args), skills (args hint + rest) and agents; collision aliases scope:name / kind:name; tokenizer for positional, key=value and quotes; bind() with helpful errors; reload(&Catalog).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Tests in commands::tests (tokenizer, binding errors, workflow/skill/agent specs, collisions, hot reload). The TUI owner still has to build input::SlashCommand and palette entries from CommandRegistry (name, description, arg_hint(), scope_label()) and call reload after Catalog::reload; listed as cross-crate need. This task adds no visible UI itself.

Audit 2026-10-24: DoD#3 (loop guard) checked. It is a process item: the notes record no failing test or check left unresolved and no fix loop, and the task's tests pass now. cargo nextest run -p harness-agent -E 'test(/^commands::/)': 8/8 pass (tokenizer, binding errors, workflow/skill/agent specs, collisions, hot reload); main CI green at cfb7027 (https://github.com/thowd22/harness/actions/runs/40048623347).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
One slash-command registry for built-ins, workflows, skills and agents with argument specs, scope-qualified collision aliases, a quoting/key=value parser with usage-bearing errors and hot reload. Verified with cargo test -p harness-agent --lib commands. Docs: docs/commands.md.
<!-- SECTION:FINAL_SUMMARY:END -->
