---
id: TASK-169
title: 'Shift+Tab cycles permission modes, with a new yolo mode'
status: To Do
assignee: []
created_date: '2026-10-29 17:54'
labels:
  - tui
  - policy
dependencies: []
priority: medium
ordinal: 183000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Switching the approval mode today means typing `/mode ask|auto-edit|auto` or using the palette. The user wants it to work like Claude Code: Shift+Tab in the chat input steps through the modes and the current one is always visible. They also want a mode above `auto`, named `yolo`, that never shows a permission prompt. `auto` still asks when a policy rule says ask or when the outward-facing action guard (TASK-28) fires; `yolo` is for runs where the user accepts that and wants no interruptions at all.

Context:
- The modes are `harness_policy::rules::Mode` (`crates/harness-policy/src/rules.rs`), ordered from most to least supervised, and are used by the ini `mode` setting, `harness run --mode`, `/mode` and the palette.
- Shift+Tab already moves focus in panels and pickers, and from the chat input it currently reaches the focus layer (`crates/harness-tui/src/chat/view.rs`). The new binding only takes over in the chat input, and has to be rebindable through `[keys]` like the other keys (TASK-33).
- The mode is shown in the status line after the model (`<model> · effort low · <mode>`, `crates/harness-tui/src/status.rs`). The user wants it at the far left, next to the working-directory indicator.
- Assumption to confirm with the user before implementing: "zero permission prompts" means every call that would ask runs without asking, including the outward guard. Explicit deny rules, scope limits and the bash sandbox are not prompts, so they still apply.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 In the chat input, Shift+Tab steps ask → auto-edit → auto → yolo and wraps back to ask; the key can be rebound in `[keys]`
- [ ] #2 Shift+Tab keeps its focus-moving behaviour in panels, pickers and dialogs
- [ ] #3 The new mode applies immediately, including to a run already in progress, and is saved with the session the same way `/mode` changes are
- [ ] #4 `yolo` is accepted everywhere a mode is: ini `mode`, `harness run --mode`, `/mode`, the palette and the `/mode` completion list
- [ ] #5 In `yolo`, no tool call, outward-facing action or subagent ever shows an approval prompt or waits for approval; tests cover a policy rule that asks, an outward guard hit and a subagent call
- [ ] #6 Choosing `yolo` shows a one-line warning that nothing will be confirmed; it gets its own warning colour in the status line
- [ ] #7 The mode indicator is the first status-line segment, left of the working directory, and stays visible when the terminal is too narrow for the other segments
- [ ] #8 Docs (commands, keybindings, policy/modes) and the CHANGELOG describe Shift+Tab and `yolo`
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
