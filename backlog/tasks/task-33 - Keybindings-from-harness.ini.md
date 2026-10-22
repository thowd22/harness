---
id: TASK-33
title: Keybindings from harness.ini
status: Done
assignee:
  - '@tui-foundation'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:30'
labels:
  - tui
  - config
milestone: m-3
dependencies:
  - TASK-32
  - TASK-2
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 33000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The user wants minimal keys and keybinds configured in the INI file. Every other action belongs in the palette.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Defaults: Enter send, Shift or Alt+Enter newline, Esc cancel or back, Tab switch view, Ctrl+P palette, Ctrl+C interrupt, Ctrl+Q quit
- [x] #2 keys section overrides defaults and conflicting bindings are reported at load
- [x] #3 A help overlay is generated from active bindings
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
1. keys::KeyChord parse/display/match (normalising shift, BackTab).
2. keys::Action (KEY_ACTIONS) + Keymap::defaults and Keymap::from_config(&Config) with per-action override, parse errors and conflict errors carrying file:line.
3. Help overlay widget generated from the active Keymap, closable by Esc and by mouse via the interaction layer.
4. Tests + docs/tui.md keys section.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/keys.rs: Action (matches harness_config KEY_ACTIONS, checked by test), KeyChord::{parse, new (normalises uppercase->shift, BackTab->shift+tab, drops shift on non-letters), from_event, matches, Display}, Keymap::{defaults, from_config, from_entries, lookup, chords, describe, help_rows}. A [keys] entry replaces that action's defaults; bad chords, chords bound to two actions and binding the fixed ctrl+z suspend key are KeyErrors with file:line. HelpOverlay renders the active bindings plus FIXED_KEYS as a modal with a focusable [ Close ] button (Esc, Enter, click on button or outside closes); opened from the footer 'keys' button or AppCommand::ShowHelp (for the palette). Tests in keys.rs and app.rs (help_overlay_by_mouse / help_overlay_by_keyboard). Docs: docs/tui.md Keys section, chord syntax added to doc-2.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Default keymap, [keys] overrides with located parse/conflict errors, and a help overlay generated from active bindings usable by mouse and keyboard. Verified by cargo test/clippy -p harness-tui.
<!-- SECTION:FINAL_SUMMARY:END -->
