---
id: TASK-34
title: Chat view
status: Done
assignee:
  - '@tui-views'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:18'
labels:
  - tui
milestone: m-3
dependencies:
  - TASK-117
  - TASK-32
  - TASK-39
priority: high
type: feature
ordinal: 34000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The main view must stay readable and fast in long sessions with heavy tool use.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Virtualized transcript keeps rendering cost constant regardless of length
- [x] #2 Streaming Markdown with lazily loaded syntax highlighting for code blocks
- [x] #3 Tool calls are one-line summaries expandable to detail; thinking follows ui show_thinking
- [x] #4 Errors and retries are shown inline without modal dialogs
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
1. chat::text/timefmt/highlight/markdown helpers. 2. chat::transcript: entries folded from RunEvents, per-entry layout cache, anchor-based virtual scrolling. 3. Streaming Markdown block cache; OnceLock language table, highlight only laid-out code. 4. Tool cards (cards module) as one-line headers expandable to detail; show_thinking modes. 5. Inline notices for errors/retries. 6. chat::view::ChatView composes it with mouse/keyboard routing; tests with TestBackend.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/chat/{text,timefmt,highlight,markdown,transcript,view}.rs. Virtualization: each Entry caches its laid-out lines keyed by (width, version, settings, focus, live spinner frame); following the tail fills from the last entry upward, scrolled-back views are anchored at (entry, line) and fill downward, so a frame costs O(viewport + scroll distance). Test rendering_cost_is_independent_of_length shows identical layout counts for 100 and 10k entries and zero layouts on a cached second frame. MarkdownCache renders completed blocks once (test streaming_renders_completed_blocks_once). Highlighting: dependency-free lexer, language table in a OnceLock, applied only when an entry is laid out (i.e. visible). Tool calls: header line is the one-line summary, Enter/click toggles detail (cards_toggle_by_click_and_by_keyboard). show_thinking hidden/collapsed/expanded tested (thinking_follows_show_thinking). Errors (EventKind::Error), NodeRetry and provider retries (Transcript::push_retry with discard_partial) render as inline notices, no modal (errors_and_retries_are_inline_notices). Validation: cargo test -p harness-tui (131 lib tests pass), cargo clippy -p harness-tui --all-targets -- -D warnings clean. Integration left to the app owner: App::render_body should delegate View::Chat to ChatView.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Virtualized, cached transcript (chat::transcript) with streaming Markdown and lazy syntax highlighting, tool cards as expandable one-line summaries, show_thinking support and inline error/retry notices; composed in chat::view::ChatView. Verified by unit + TestBackend tests in harness-tui (131 pass) and clippy -D warnings; documented in docs/tui-views.md.
<!-- SECTION:FINAL_SUMMARY:END -->
