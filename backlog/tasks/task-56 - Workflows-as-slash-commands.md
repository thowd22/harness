---
id: TASK-56
title: Workflows as slash commands
status: Done
assignee:
  - '@workflows-subagents'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - workflow
  - commands
milestone: m-5
dependencies:
  - TASK-55
  - TASK-45
priority: high
type: feature
ordinal: 56000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Workflows should be invoked like any command.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Each workflow registers /command (default name) with inputs mapped from arguments
- [x] #2 Missing required inputs are prompted for in the palette
- [x] #3 Help is generated from the workflow description and inputs
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
1. harness_workflow::commands: command(wf) -> WorkflowCommand (name = command:, default name; args from inputs; usage()).
2. parse_args(wf, args): positional words, joined tail, --name value/--name=value/name=value, bool flags, list values, type conversion; ParsedArgs.missing with prompt()/hint() for the palette; complete() fills answers and defaults.
3. help(wf) from description, inputs and steps.
4. TUI palette/slash registry wiring by harness-tui.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Library side done in crates/harness-workflow/src/commands.rs (3 tests). Remaining (AC#2, DoD#4, harness-tui / slash registry owner): when /<command> is invoked, call harness_workflow::commands::parse_args(&wf, args). If ParsedArgs::missing is non-empty, open palette prompts using CommandArg::prompt() as the name and CommandArg::hint() as the hint (palette::ArgSpec{name, required:true, hint}), then call commands::complete(&wf, &parsed, &answers) and start Executor::run. Show commands::help(&wf) for /help <command>. Tests are needed for both mouse and keyboard paths. Then check AC#2 and DoD#4. Validation: cargo test -p harness-workflow passes; clippy -D warnings clean.

Integration: /<workflow> with missing inputs opens palette prompts (registry MissingArg routes to the workflow prompt); answers complete and run. Tests: tui workflow_commands_prompt... (keyboard) and app palette prompt tests (mouse and keyboard).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: /<workflow> with missing inputs opens palette prompts (registry MissingArg routes to the workflow prompt); answers complete and run. Tests: tui workflow_commands_prompt... (keyboard) and app palette prompt tests (mouse and keyboard). Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
