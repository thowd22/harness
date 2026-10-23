---
id: TASK-150
title: 'Stall indicator: don''t report long-running tools as ''no data'''
status: To Do
assignee: []
created_date: '2026-10-23 15:35'
labels:
  - tui
  - indicators
dependencies: []
priority: high
ordinal: 154000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The activity line shows `⚠ no data for {secs}s ({source})` (crates/harness-tui/src/indicators.rs, Activity::stall, after `[ui] stall_after`, default 20s) when no provider data or tool output arrives. During long, legitimate tool runs (a real session: `cargo test --workspace` on a large repo for 8+ minutes, busy at 350% CPU with its children making progress) this reads as if harness or the model is stuck, which is misleading.

Distinguish 'stalled' from 'busy but quiet':
- Provider stream silence (waiting for model tokens) keeps the stall warning, but says what it is waiting on (e.g. 'model hasn't responded for 40s').
- A running tool whose output is quiet is shown as working, not stalled: elapsed time against its timeout (`bash_timeout` / call timeout, e.g. 'running cargo test · 4m12s of 10m'), plus liveness from the process group where available (CPU time advancing, child processes alive) so a hung tool (no CPU, no output) can still be flagged.
- Remote tools report the same liveness through the helper where possible.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 While a tool runs and its process group is using CPU or has live children, the activity line shows the tool as running with elapsed time and its timeout, and no 'no data' warning
- [ ] #2 A tool with no output and no CPU progress for stall_after is flagged as possibly hung, naming the tool and suggesting cancel
- [ ] #3 Provider-stream silence still warns, worded as waiting on the model, with the elapsed time
- [ ] #4 Liveness sampling is cheap (bounded polling, no per-frame syscalls) and works on Linux and macOS; when unavailable it falls back to elapsed/timeout wording
- [ ] #5 docs/tui.md and the [ui] stall_after docs explain the states; unit tests cover each state with injected clocks and liveness
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
