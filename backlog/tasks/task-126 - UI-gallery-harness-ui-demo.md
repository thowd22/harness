---
id: TASK-126
title: 'UI gallery: harness ui-demo'
status: Done
assignee:
  - '@visibility-tui'
created_date: '2026-10-22 14:43'
updated_date: '2026-10-22 19:17'
labels:
  - tui
  - testing
milestone: m-6
dependencies:
  - TASK-121
  - TASK-122
  - TASK-123
  - TASK-124
  - TASK-125
  - TASK-14
  - TASK-64
documentation:
  - backlog/docs/doc-7 - Visual-Design-Dracula-Theme-Indicators-Tool-Cards.md
priority: low
type: feature
ordinal: 126000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Visual polish needs a fast review loop. A demo mode that renders every component with scripted data makes visual review, screenshots and styled snapshot tests easy.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 harness ui-demo plays a scripted run through the fake provider, showing cards, indicators, timestamps, map, goal bar, palette, approvals and subagent cards
- [x] #2 Each component in the gallery has a styled snapshot test
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
1. demo/events.rs EventBuilder + NodeSpec for scripted runs. 2. demo::scripted_run() and gallery chat_events(). 3. demo/gallery.rs Gallery with a page per component (chat cards/indicator/timestamps/subagent cards, map, detail, goal bar, palette, approvals, diff, inspector), clickable tabs and keys. 4. FAKE_SCRIPT for the fake provider. 5. Styled snapshot per page.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_tui::demo (events::{EventBuilder, NodeSpec, usage, T0}, scripted_run, gallery::{Gallery, Page, GalleryOutput, chat_events, approval_prompt, FAKE_SCRIPT}). Tests: one styled insta snapshot per page (gallery_chat, gallery_map, gallery_detail, gallery_goal_bar, gallery_palette, gallery_approvals, gallery_diff, gallery_inspector), pages_show_their_components, tabs_switch_by_click_and_keyboard, fake_script_parses. Remaining for AC1 + DoD1: the harness binary (not owned here) must add 'harness ui-demo': run the TUI with harness_tui::demo::gallery::Gallery (render + handle until GalleryOutput::Quit), and/or play FAKE_SCRIPT through harness_providers::fake::FakeProvider and the real AgentLoop into a ChatView/MapView, with a binary-level test.

Integration: `harness ui-demo` plays a scripted fake-provider session through the real TUI (cards, indicators, timestamps, map, goal bar, palette, approvals, subagent cards); --gallery and --print. Test: tests/ui_demo.rs ui_demo_plays_the_scripted_session; pty smoke run exits 0 and restores the terminal.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: `harness ui-demo` plays a scripted fake-provider session through the real TUI (cards, indicators, timestamps, map, goal bar, palette, approvals, subagent cards); --gallery and --print. Test: tests/ui_demo.rs ui_demo_plays_the_scripted_session; pty smoke run exits 0 and restores the terminal. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
