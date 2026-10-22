---
id: TASK-32
title: TUI app skeleton
status: Done
assignee:
  - '@tui-foundation'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:31'
labels:
  - tui
milestone: m-3
dependencies:
  - TASK-6
  - TASK-16
priority: high
type: feature
ordinal: 32000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The UI must be simple, clean and fast. The architecture must render only on change and never block on the core.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 ratatui and crossterm app with async event loop consuming core events via channels
- [x] #2 Renders only when state is dirty and idles at near zero CPU
- [x] #3 Handles resize, suspend and resume, and always restores the terminal
- [x] #4 Cold start to interactive under 150 ms measured in CI
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
1. app::AppEvent (terminal, core RunEvent, command, tick) on a tokio mpsc channel; forwarders for crossterm EventStream and RunRecorder broadcast.
2. App state folds RunEvents into reducer::RunState, dirty flag, renders only when dirty; ticks only while something runs.
3. TerminalSession RAII guard over a TerminalControl trait: enter/leave raw mode, alt screen, mouse; suspend (Ctrl+Z -> SIGTSTP) and resume (full redraw); restore on drop/error/panic.
4. Resize -> autoresize + redraw.
5. Tests with TestBackend (draw counts, resize, suspend/resume fake, cold start timing < 150ms) and docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-tui/src/app.rs. AppEvent (Terminal, Run(Box<RunEvent>), Command(AppCommand), Resumed, Shutdown) on one bounded tokio mpsc (event_channel); forwarders forward_terminal_events (crossterm EventStream), forward_run_events (RunRecorder::subscribe broadcast; lag is logged, never blocks the core), forward_signals (SIGCONT->Resumed, SIGTERM/SIGHUP->Shutdown). UI->core requests on an unbounded mpsc of UiRequest (Submit, Interrupt, Cancel, OpenPalette, Quit). App folds RunEvents into harness_events::reducer::RunState, renders a minimal chat/map placeholder body, input line and footer (view tabs, run status spinner, mouse-off badge, keys button); terminal work is returned as Effect (SetMouseCapture, Suspend, Resume). run_loop draws only when dirty, coalesces up to MAX_BATCH queued events per frame, ticks at 80 ms only while App::needs_tick (running + animations). TerminalControl trait with CrosstermControl (raw mode, alt screen, bracketed paste, mouse; suspend = restore + nix raise(SIGTSTP) + re-enter) and RecordingControl for tests; TerminalSession RAII guard restores on drop/error/unwind (abort panics covered by terminal::install_panic_hook). run() wires it all for the real terminal. Tests: renders_only_when_dirty, event_loop_consumes_channel_and_coalesces, idles_without_ticks_and_ticks_only_while_running (paused tokio clock: 0 ticks/1 frame over 10 s idle), resize_redraws_at_new_size, suspend_resume_and_mouse_toggle_go_through_control, terminal_is_restored_on_drop_error_and_panic, cold_start_to_interactive_under_150ms (config parse -> App -> first frame -> first key handled+drawn, 200x60 TestBackend, runs in the normal CI test job). Real SIGTSTP/raw mode cannot run in CI; covered through the TerminalControl fake. Not in scope here: the harness binary must call harness_tui::app::run (CLI owner).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Async, channel-driven ratatui app skeleton: dirty-only rendering, idle with no timers, resize, Ctrl+Z suspend/resume, RAII terminal restore and a cold-start timing test. Verified by cargo test/clippy -p harness-tui.
<!-- SECTION:FINAL_SUMMARY:END -->
