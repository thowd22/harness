---
id: TASK-40
title: Non-modal notifications
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:19'
labels:
  - tui
milestone: m-3
dependencies:
  - TASK-117
  - TASK-32
priority: low
type: feature
ordinal: 40000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Background events such as memory updates, remote disconnects or budget warnings need to be noticed without interrupting work.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Toasts appear and expire without stealing focus
- [x] #2 A notification history is available from the palette
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
1. notify::Notifications: history (bounded), toasts with TTL, dismiss, expiry. 2. event_notification for memory changes, recoverable errors, finished runs. 3. Toast rendering top-right registering clickable regions without changing focus. 4. History modal opened from the palette entry notify.history. 5. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/notify.rs and ChatView wiring (palette id notify.history). Toasts: TOAST_TTL 6s, MAX_TOASTS 3, click dismisses; focus is never moved (toasts_do_not_steal_focus_and_dismiss_on_click, notification_history_from_palette_and_toasts_keep_focus). History: modal with Up/Down, Esc and a clickable [ close ] (history_overlay_keyboard_and_mouse). Budget warnings and remote disconnects have no dedicated event kinds yet; callers can push them with ChatView::notify / Notifications::push, and recoverable EventKind::Error becomes a warning toast.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Non-focus-stealing toasts that expire, plus a notification history overlay opened from the palette, mouse and keyboard operable. Verified by harness-tui tests; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
