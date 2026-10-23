---
id: TASK-141
title: Fuzzy ranked slash-command completion
status: To Do
assignee: []
created_date: '2026-10-23 02:31'
updated_date: '2026-10-23 02:31'
labels:
  - tui
  - commands
  - interaction
milestone: m-3
dependencies:
  - TASK-35
  - TASK-36
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
priority: medium
type: enhancement
ordinal: 141000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Typing / opens a completion dropdown, but it only matches commands whose name starts with or contains the typed text, sorted alphabetically: /mdl finds nothing. Users expect oh-my-pi-style fuzzy search. The Ctrl+P palette already uses the nucleo fuzzy matcher, so the dropdown should share it.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The slash dropdown uses the same nucleo matcher as the palette: gaps are allowed (/mdl matches /model) and results are ranked by match score, with exact and prefix matches first
- [ ] #2 Matched characters are highlighted with theme tokens
- [ ] #3 Descriptions are searched too, ranked below name matches, so /review finds a workflow whose description mentions review
- [ ] #4 Namespaced commands (global:name, project:name) match on the bare name and on the full name
- [ ] #5 Ranking is stable for equal scores and updates within one frame per keystroke with 1000 commands
- [ ] #6 Arrow keys, Tab, Enter, Esc and clicks keep working as today, and tests cover gapped, prefix, description and namespaced matches
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
