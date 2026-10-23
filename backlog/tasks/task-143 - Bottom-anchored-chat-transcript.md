---
id: TASK-143
title: Bottom-anchored chat transcript
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 04:24'
updated_date: '2026-10-23 06:44'
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
- [x] #1 The transcript is anchored to the bottom: when it is shorter than the pane, messages sit directly above the input with empty space above them
- [x] #2 New output appears at the bottom and the view follows it while the user is at the bottom
- [x] #3 Scrolling up (wheel, PgUp, keyboard) pauses following and keeps the viewed lines stable while new output arrives; a 'new messages' indicator appears and clicking it or End returns to the bottom and resumes following
- [x] #4 Resizing the terminal keeps the bottom anchored and does not jump the reading position
- [x] #5 Rendering stays virtualized and within the frame budget for long transcripts
- [x] #6 Styled snapshot tests cover short, long, scrolled-up and resized transcripts
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
1. Transcript: bottom-anchor rendering (pad above a short transcript, hits shifted to screen rows), pin() helper so a transcript that fits never leaves following, unseen flag for output below a pinned view (push + apply tail fingerprint), width-change keeps pinned entry/relative offset, pinned view that shows the tail follows again.
2. View: draw lines below the padding, empty hint on the bottom row, clickable/focusable '↓ new messages · end' button (ChatAction::Follow), End/Ctrl+End follow keys that do not steal End from the input mid-line.
3. Tests: transcript unit tests + view tests with styled snapshots (short, long, scrolled-up, resized), budget test best-of-5.
4. Docs: docs/tui-views.md, docs/tui.md, doc-7, CHANGELOG, keys help row.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented per plan. Transcript::render keeps its (lines, hits) signature; hits are now in screen rows including the padding (Transcript::pad()); one existing test (memory undo) now indexes lines by rect.y - pad, same assertion. Reviewed and accepted snapshot changes: app keys-help overlay (new 'end  jump to the latest message' row, hint moved to bottom row), demo gallery chat (content moved to the bottom). Validation: cargo nextest run -p harness-tui chat:: keys:: app:: demo:: -> 124 passed; full harness-tui run has 3 failures in history.rs/palette.rs that belong to the names lane's in-progress edits (agent names, duration format), not this change. clippy -p harness-tui --all-targets -D warnings clean; rustfmt applied to touched files only (cargo fmt --all would touch other lanes' files).

Integration (2026-10-23): workspace green locally after all three lanes merged in the tree: cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1538 passed, 8 skipped (pre-existing gated tests); cargo test --doc ok; cargo deny check ok; tests/install-test.sh ok; shellcheck on install.sh, scripts/*.sh, tests/*.sh clean. The earlier history.rs/palette.rs failures are green. No tests removed (55 #[test] added, 0 removed). DoD #1 left unchecked until CI passes on the pushed commit.

Green in CI on ubuntu and macos (https://github.com/thowd22/harness/actions/runs/39911543976) and in the v0.2.0 release gate (https://github.com/thowd22/harness/actions/runs/39911979325); shipped in https://github.com/thowd22/harness/releases/tag/v0.2.0 and verified with the installed binary.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Chat transcript is now bottom-anchored chat-app style: a short conversation is padded above so the newest message sits just above the input; following new output at the bottom; scrolling up (wheel/PgUp) pins the viewed lines, and new output raises a clickable, keyboard-focusable '↓ new messages · end' button; click, Enter on it, End (when the input would not use it), Ctrl+End, scrolling down, palette follow or sending resume following. Resizes keep a following view at the bottom and a pinned view on the same entry (relative offset kept on width change); a pinned view that can show the tail follows again. Rendering stays virtualized (existing cost test + new 10k-entry best-of-5 <16ms frame budget test). Verified with new transcript unit tests and chat view tests including styled snapshots chat_short, chat_long, chat_scrolled_up, chat_resized. Docs: docs/tui-views.md, docs/tui.md, doc-7, CHANGELOG Unreleased, keys help.
<!-- SECTION:FINAL_SUMMARY:END -->
