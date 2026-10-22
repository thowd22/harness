---
id: TASK-36
title: Command palette
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:18'
labels:
  - tui
milestone: m-3
dependencies:
  - TASK-117
  - TASK-32
  - TASK-33
  - TASK-39
priority: high
type: feature
ordinal: 36000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
One palette replaces most keybindings so users learn only one key.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Ctrl+P opens a fuzzy palette over every action: built-ins, workflows, skills, agents, runs, remotes, settings
- [x] #2 Entries show description, source scope and keybinding if any
- [x] #3 Actions with arguments prompt for them inline
- [x] #4 Palette opens and filters in under 16 ms with 1000 entries
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
1. palette::Palette with entries, nucleo-matcher scoring over pre-converted Utf32 haystacks, selection and inline argument prompting. 2. Entry builders: builtin_entries (with keybindings and harness_policy mode entries), catalog_entries (workflows/skills/agents with scope), run_entries, remote_entries. 3. Modal render with clickable rows and match highlighting. 4. ChatView: Ctrl+P (Action::Palette) toggles; built-in ids handled locally, others returned as ChatOutput::Run. 5. Perf test with 1000 entries.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/palette.rs and the ChatView wiring. Sources covered: Builtin, Workflow, Skill, Agent, Run, Remote, Setting (covers_every_source_kind loads a real harness_config Catalog). Rows show title, description, kind·scope, keybinding. Args prompt inline (arguments_are_prompted_inline). opens_and_filters_1000_entries_within_a_frame asserts open+render and each keystroke filter+render < 16 ms in a debug build. Mode entries come from harness_policy::approvals::palette_entries and are applied with ModeState::apply_palette_command in ChatView (palette_mode_changes_update_the_footer) - this is the TUI half of TASK-27 AC#1. Ctrl+P handling is in ChatView (ctrl_p_opens_palette_filters_and_runs_by_keyboard_and_mouse); app.rs still sends UiRequest::OpenPalette and must delegate to ChatView when integrated.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fuzzy command palette over built-ins, workflows, skills, agents, runs, remotes and settings with description/scope/keybinding, inline argument prompts, mouse+keyboard, < 16 ms with 1000 entries. Verified by harness-tui tests; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
