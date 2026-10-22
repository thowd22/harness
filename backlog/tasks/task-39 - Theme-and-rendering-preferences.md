---
id: TASK-39
title: Theme system with Dracula default
status: Done
assignee:
  - '@tui-foundation'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:30'
labels:
  - tui
milestone: m-3
dependencies:
  - TASK-32
  - TASK-2
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 39000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The UI should look polished and consistent, using the Dracula color scheme by default. Components must use semantic tokens (ok, error, running, accent and so on) rather than raw colors, so the look stays consistent and users can override colors in harness.ini.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Semantic token set from the visual design doc with Dracula as the default mapping, including card tints and diff colors
- [x] #2 Truecolor when COLORTERM advertises it, otherwise nearest-256 and then 16-color fallbacks that keep status colors distinguishable
- [x] #3 NO_COLOR and ui theme none disable color while keeping icons and layout
- [x] #4 ui background chooses between painting the Dracula background and keeping the terminal background
- [x] #5 Every token can be overridden in the theme section of harness.ini, and invalid hex values are load errors with line numbers
- [x] #6 No component uses a raw color, enforced by a lint or test
- [x] #7 Styled snapshot tests render key components in truecolor, 256 and no-color modes
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
1. theme::Token semantic token set (doc-7) with ini names, Rgb + blending for card tints and diff backgrounds.
2. Palette::dracula default; Palette overrides from Config.theme (unknown token -> ThemeError with file:line from Config.origins; hex validation already done by harness-config with line numbers).
3. ColorDepth::detect(env) (NO_COLOR > COLORTERM truecolor/24bit > TERM *256color > 16); nearest-256 via xterm cube/grey ramp, fixed 16-colour semantic map keeping status colours distinct.
4. Theme::style/fg/bg/base API honouring [ui] theme=none and background=theme|terminal.
5. Test that scans src/ for raw Color:: usage outside theme.rs.
6. Styled insta snapshots of a sample component in truecolor/256/none.
7. docs/tui.md theme section.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/theme.rs: Token enum (29 semantic tokens incl. card.* tints and diff.* colours; aliases/tints follow overridden base tokens), Rgb (parse_hex, blend_over), Palette::dracula/with_overrides, ColorDepth::detect_with (NO_COLOR > COLORTERM truecolor|24bit > TERM *256color > 16), nearest_256, fixed 16-colour semantic map, Theme::{from_config_with, color, fg, bg, fg_on, base, focused, border}. Hex validation with file:line is done by harness-config (verified by test here); unknown token names are ThemeErrors with file:line from Config.origins. Raw-colour lint: theme::tests::no_raw_colors_outside_theme scans every src/**/*.rs except theme.rs for Color:: and Stylize colour shorthands (escape hatch: a 'raw-color-ok' comment on the line). Styled insta snapshots (interaction::testing::styled_snapshot) for a card/diff/status sample in truecolor/256/16/none and the app frame with help overlay in truecolor/256/none. Docs: docs/tui.md, token list added to doc-2. Validation: cargo test -p harness-tui (54 pass), cargo clippy -p harness-tui --no-deps --all-targets -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Semantic token theme with Dracula default, depth fallbacks, NO_COLOR/theme=none, background policy, [theme] overrides with located errors, raw-colour lint test and styled snapshots. DoD#4 N/A (no interactive element). Verified by cargo test/clippy -p harness-tui.
<!-- SECTION:FINAL_SUMMARY:END -->
