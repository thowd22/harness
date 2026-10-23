---
id: TASK-143
title: Bottom-anchored chat transcript
status: To Do
assignee: []
created_date: '2026-10-23 04:24'
updated_date: '2026-10-23 04:24'
labels:
  - tui
  - interaction
milestone: m-3
dependencies:
  - TASK-34
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
priority: high
type: enhancement
ordinal: 143000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The main chat fills and scrolls from the top down, like a document. Users expect chat behaviour: the newest message sits just above the input, a short conversation hugs the bottom of the pane, and older messages scroll away upward. Following the stream must stay effortless while reading history must not be interrupted.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 The transcript is anchored to the bottom: when it is shorter than the pane, messages sit directly above the input with empty space above them
- [ ] #2 New output appears at the bottom and the view follows it while the user is at the bottom
- [ ] #3 Scrolling up (wheel, PgUp, keyboard) pauses following and keeps the viewed lines stable while new output arrives; a 'new messages' indicator appears and clicking it or End returns to the bottom and resumes following
- [ ] #4 Resizing the terminal keeps the bottom anchored and does not jump the reading position
- [ ] #5 Rendering stays virtualized and within the frame budget for long transcripts
- [ ] #6 Styled snapshot tests cover short, long, scrolled-up and resized transcripts
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
