---
id: TASK-169
title: 'Shift+Tab cycles permission modes, with a new yolo mode'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-29 17:54'
updated_date: '2026-10-29 18:20'
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
- [x] #1 In the chat input, Shift+Tab steps ask → auto-edit → auto → yolo and wraps back to ask; the key can be rebound in `[keys]`
- [x] #2 Shift+Tab keeps its focus-moving behaviour in panels, pickers and dialogs
- [x] #3 The new mode applies immediately, including to a run already in progress, and is saved with the session the same way `/mode` changes are
- [x] #4 `yolo` is accepted everywhere a mode is: ini `mode`, `harness run --mode`, `/mode`, the palette and the `/mode` completion list
- [x] #5 In `yolo`, no tool call, outward-facing action or subagent ever shows an approval prompt or waits for approval; tests cover a policy rule that asks, an outward guard hit and a subagent call
- [x] #6 Choosing `yolo` shows a one-line warning that nothing will be confirmed; it gets its own warning colour in the status line
- [x] #7 The mode indicator is the first status-line segment, left of the working directory, and stays visible when the terminal is too narrow for the other segments
- [x] #8 Docs (commands, keybindings, policy/modes) and the CHANGELOG describe Shift+Tab and `yolo`
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Policy: add Mode::Yolo after Auto (ALL, as_str/parse "yolo", palette description, ModeState::cycle ask→auto-edit→auto→yolo→ask). Commands default to allow in auto and yolo. PolicyEngine::decide and StagingGuard::decide turn any remaining Ask verdict into Allow in yolo, with a rule naming yolo and the reason that would have asked; Deny is unchanged. Hygiene scope: yolo flags like auto.
2. Keys: new Action::CycleMode (`cycle_mode`, default shift+tab). In the chat view, when the input has focus and does not use the key itself, it cycles the mode (status + ModeChanged); a focused card, panels, pickers, the palette and approvals keep Shift+Tab for focus. FIXED_KEYS text updated.
3. Driver: ModeChanged already sets core + opts.mode (session); add a one-line warning notice when the mode becomes yolo (also for /mode yolo and the palette).
4. Status line: mode becomes the first segment, left of the cwd, highest priority so it is never dropped; yolo uses the error colour. Model segment loses the mode.
5. Spellings: /mode choices and usage, harness run --mode help, ini docs, agent YAML schema unchanged (agents can only tighten).
6. Tests: policy (rule ask, outward guard, staging, deny kept), dispatch/subagent gate in yolo with a UI that panics if asked, chat Shift+Tab cycling and focus behaviour, status line order/narrow width, /mode yolo warning. Docs + CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented:
- Policy: Mode::Yolo (rules + config enums). Verdict::in_mode turns any Ask into Allow under yolo (rule `mode yolo`, reason names the rule that would have asked), applied in PolicyEngine::decide and StagingGuard::decide, so rules, outward guard, staging guard and MCP never ask; deny/protected paths unchanged. Hygiene scope flags in yolo like auto. ModeState::cycle ask→auto-edit→auto→yolo→ask.
- Keys: `cycle_mode` (shift+tab) cycles from the chat input; `focus_transcript` (alt+up) replaces Shift+Tab as the input→transcript focus key (user chose Alt+Up when asked). Panels, pickers, dialogs and a focused card keep Shift+Tab for focus.
- Chat view syncs its mode on /mode (ChatView::set_mode) so Shift+Tab starts from the current mode.
- Warning (Level::Warning "yolo mode", YOLO_WARNING) on Shift+Tab/palette, /mode yolo, TUI start in yolo; headless prints it on stderr.
- Status line: mode is the first segment (priority 11, never dropped), yolo in the error colour; details panel lists "Mode:".
- Agent YAML rejects `mode: yolo` (an agent can only tighten the session).
Verification: new tests yolo_never_asks_but_still_denies, yolo_sweeps_staging..., yolo_gates_never_ask (NeverAskUi panics if asked; main + capped subagent gate), shift_tab_in_the_input_cycles_the_mode, mode_comes_first_survives_narrow_terminals_and_yolo_stands_out, yolo_warns_and_runs_without_prompts (driver), yolo_mode_runs_what_would_ask_and_warns (headless, --mode and ini); updated focus tests to Alt+Up; snapshots reviewed (status line order, help rows, palette row). fmt, clippy -D warnings, nextest workspace 2160 passed / 24 skipped, doc tests, hermetic-guard, bench-guard ok. Not committed yet.
<!-- SECTION:NOTES:END -->
