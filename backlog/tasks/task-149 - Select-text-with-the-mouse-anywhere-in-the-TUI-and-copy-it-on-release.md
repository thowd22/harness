---
id: TASK-149
title: Select text with the mouse anywhere in the TUI and copy it on release
status: To Do
assignee: []
created_date: '2026-10-23 15:35'
labels:
  - tui
  - clipboard
dependencies: []
priority: high
ordinal: 153000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The TUI captures the mouse (EnableMouseCapture in crates/harness-tui/src/app.rs) so buttons are clickable, which disables the terminal's native drag-to-select. Today the only way to copy is the copy buttons on tool cards (`⧉ copy output`) and the copy action; dragging over text does nothing. The user expects copy-on-select everywhere: press, drag over any text, release, and it is in the clipboard.

Implement in-app text selection: track mouse down/drag/up over rendered text, highlight the selection (theme token, works in both themes), and on release copy the selected text through the existing clipboard backend (crates/harness-tui/src/clipboard.rs, OSC 52 by default, `[ui] clipboard`) with a short toast. It must work in every view that shows text: chat transcript (including scrolled content and wrapped lines, copying the logical text without wrap breaks or UI chrome like borders and gutters), tool cards and their output, agent map and node detail, diff view, panels/modals (/mcp, /model, sessions), and the input box. Drag past the top/bottom edge auto-scrolls. Clicks without drag still activate buttons. Pasting (bracketed paste, middle-click where the terminal supports it) keeps working into the input.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Press-drag-release over text in any view (chat, tool cards, map/node detail, diff, panels and modals, input) highlights the selection and copies exactly that text to the clipboard on release, with a toast
- [ ] #2 Copied text is the logical content: soft-wrapped lines are rejoined, and borders, gutters, icons and button labels are not included
- [ ] #3 Dragging past the view edge scrolls and extends the selection; a click without drag still activates buttons and focus as before
- [ ] #4 Double-click selects a word and triple-click a line; Esc or a click elsewhere clears the selection
- [ ] #5 Keyboard path: a selection mode (palette entry and keybind) moves a cursor and extends the selection with Shift+arrows, Enter copies
- [ ] #6 Paste into the input keeps working (bracketed paste); docs/tui.md and the clipboard docs describe copy-on-select
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
