---
id: TASK-136
title: Copy and paste in the TUI
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:52'
updated_date: '2026-10-23 04:08'
labels:
  - tui
  - interaction
milestone: m-3
dependencies:
  - TASK-35
  - TASK-117
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 136000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Copy and paste must work reliably. Mouse capture (needed for clickable elements) disables the terminal's native text selection, so the harness must provide its own copy path, and pastes of code and logs must arrive intact. Copy must also work over SSH, where system clipboard tools are unavailable, which is what the OSC 52 escape sequence is for.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Bracketed paste delivers multi-line text, tabs, unicode and very large pastes into the input exactly, without triggering keybindings or sending early; large pastes collapse to a placeholder that expands and is sent in full
- [x] #2 Paste works with mouse capture on and off, and in the approval prompt, palette filter and dialogs that accept text
- [x] #3 Copy actions for message, code block, tool call output, diff and log entry are available by click, keyboard and palette, and report success in a toast
- [x] #4 Copy uses OSC 52 by default (works over SSH and tmux with passthrough) and falls back to wl-copy, xclip, xsel or pbcopy when configured; ui clipboard = osc52, system or auto in harness.ini
- [x] #5 Copied text is plain (no ANSI codes, no box-drawing borders, no line numbers unless requested) and secrets are redacted as everywhere else
- [x] #6 A keyboard text-selection mode in the transcript lets users select an arbitrary range and copy it; ui mouse = false still restores native selection
- [x] #7 Pseudo-terminal tests send bracketed-paste sequences and assert the input contents, and assert the exact OSC 52 sequence emitted for copy actions
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
1. Validate current behaviour in a real pty (python pty + pyte driving target/debug/harness ui-demo and a fake-script session with temp XDG dirs).
2. Clipboard module (harness-tui/src/clipboard.rs): OSC 52 (tmux passthrough), system tools (wl-copy/xclip/xsel/pbcopy) with fallback, auto; plain-text cleanup and redaction; [ui] clipboard in harness-config.
3. Copy targets: copy buttons on message headers, code blocks (markdown header line), tool cards (copy output / copy diff), detail header, diff header; copy/select key actions; palette entries.
4. Keyboard selection mode in the transcript with plain-line extraction.
5. Paste fixes: tabs rendering, private-use chars, palette arg prompt, palette over map/screens, memory search, paste from map.
6. App wiring: Effect::Copy through the Clipboard with toasts (also over screens), redactor from the session.
7. Tests: unit, TestBackend view/app tests, pty integration test (crates/harness/tests/tui_pty.rs). Docs: docs/tui.md, tui-views, guide, doc-2, doc-7, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
PTY validation before changes (python pty + pyte on target/debug/harness ui-demo and --fake-script):
- WORKED: bracketed paste of multi-line/unicode text reached the input and was recorded exactly (user_message event); a paste containing y/n while an approval waited did not answer it; large pastes collapsed.
- BROKEN/MISSING: tabs in the input were invisible (zero-width cell) and the cursor was off; a paste while the palette was open over the map or a screen was dropped; a paste into a palette argument prompt went to the filter; pastes on the map or into the memory search were dropped; chars in U+F0000.. were taken for paste placeholders; copy existed only as the node-detail palette entry 'Copy entry' (raw OSC 52, no toast, no redaction, read line numbers included); no copy for messages, code blocks, tool output or diffs; no keyboard selection; no [ui] clipboard.
Decisions: select key default ctrl+t (ctrl+s collided with existing key-conflict tests where users bind send = ctrl+s). Overlay toasts are drawn over the map/screens (below the header row) and clickable. plain_line() strips borders/gutters/buttons for selection copies. Copy buttons are separate focusable elements, so Shift+Tab from the input now passes a card's copy buttons before the card (test updated). Snapshots changed intentionally (copy buttons, help overlay rows, detail/diff header buttons) and were reviewed before accepting.
Also fixed harness-eval defaults::check_changelog: it matched the '## Defaults' mention in the CHANGELOG intro instead of the section heading.
Validation after: cargo test -p harness-tui (262 lib + integration ok), cargo test -p harness (incl. new tests/tui_pty.rs, 3 consecutive green runs), cargo test -p harness-config, clippy -D warnings clean for harness-tui/harness-config/harness-eval (harness bin clippy currently fails only on another run's unused wiring fns), workspace tests: only defaults_changelog fails because the defaults fingerprint also moves with the concurrent provider/config run; regenerate the last ## Defaults entry fingerprint after both runs land.

DoD #1 left unchecked: tests pass locally; the CI run awaits the push.

Review follow-up (2026-10-22): both findings are only that DoD #1 waits for CI. That is correct, and DoD #1 stays unchecked until the first CI run after the push. Re-verified locally: cargo test -p harness-tui (262 lib tests plus integration tests, all passing), cargo test -p harness --test tui_pty (2/2), clippy -D warnings clean for harness-tui and harness with all targets (the earlier harness unused-fn warnings from the other run are gone). cargo fmt --all --check fails only in harness-config/src/ini/config.rs (compression_below_mbps) and harness-remote link.rs/sync.rs. Those belong to the concurrent remote run and have no TASK-136 files, so I left them for that run to format. No code changes were needed.

Verified in CI on GitHub: green on ubuntu-latest and macos-latest at c3a4fa8 (https://github.com/thowd22/harness/actions/runs/39896376900) and 84d24e7 (https://github.com/thowd22/harness/actions/runs/39897339759); also green inside the v0.1.0 release gate (https://github.com/thowd22/harness/actions/runs/39896827525).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Copy and paste in the TUI: new harness-tui clipboard module (OSC 52 with tmux passthrough, wl-copy/xclip/xsel/pbcopy with fallback, [ui] clipboard = osc52|system|auto, plain text + session redaction); copy buttons on messages, code blocks, tool cards (output/diff), node detail and diff review; copy (ctrl+y) and select (ctrl+t) key actions; palette copy entries; keyboard selection mode in the transcript; toasts also over map/screens; paste fixes (tabs, private-use chars, palette argument prompt, palette over any view, memory search, paste from the map). Verified by unit/TestBackend tests and the pty integration test crates/harness/tests/tui_pty.rs (exact user_message text, exact OSC 52 bytes, no mouse capture with mouse = false).
<!-- SECTION:FINAL_SUMMARY:END -->
