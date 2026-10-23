---
id: TASK-150
title: 'Stall indicator: don''t report long-running tools as ''no data'''
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 15:35'
updated_date: '2026-10-23 19:22'
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
- [x] #1 While a tool runs and its process group is using CPU or has live children, the activity line shows the tool as running with elapsed time and its timeout, and no 'no data' warning
- [x] #2 A tool with no output and no CPU progress for stall_after is flagged as possibly hung, naming the tool and suggesting cancel
- [x] #3 Provider-stream silence still warns, worded as waiting on the model, with the elapsed time
- [x] #4 Liveness sampling is cheap (bounded polling, no per-frame syscalls) and works on Linux and macOS; when unavailable it falls back to elapsed/timeout wording
- [x] #5 docs/tui.md and the [ui] stall_after docs explain the states; unit tests cover each state with injected clocks and liveness
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
1. harness-tools: new liveness module sampling a process group's CPU time, process count and runnable count (Linux /proc, macOS/other unix ps fallback, None when unavailable); pure parsers with unit tests.
2. LocalExec samples the group every LIVENESS_INTERVAL (3s, spawn_blocking, never per frame) and feeds an optional liveness channel on ExecHandle; ExecHandle::next_event yields output or liveness.
3. bash tool emits ToolEvent::Progress (timeout at start, then each liveness sample); dispatch maps it to a new additive EventKind::ToolProgress { call_id, timeout_ms, liveness }.
4. Remote: helper forwards samples as an ephemeral exec.liveness notification; client routes it into the job's ExecHandle. Shell fallback and old helpers send none -> unknown liveness.
5. TUI Activity: running tool shows 'elapsed of timeout' and cpu, no warning; with known liveness and no output/CPU/runnable process for max(stall_after, 6s) a soft 'quiet for Xs (no CPU)' hint naming the tool and the interrupt key; unknown liveness never warns (elapsed/timeout wording); provider silence warns 'waiting on the model: no data for Xs'. Title follows.
6. Tests: injected clocks + fake samples for each state; real local bash busy vs sleep CPU test; remote helper forwarding test; parser tests.
7. Docs: docs/tui-views.md activity line, docs/tui.md, docs/events.md, docs/tools.md/remote.md, [ui] stall_after comments in doc-2 and reference ini, doc-7 wording.

8. Review fixes: (a) UI approval envelope saves the running phase via Activity::enter_approval and approvals_answered restores it (Activity::approval_answered) instead of forcing Sending; (b) Activity tracks in-flight calls (ToolCall seen, ToolResult not yet) with tool/summary, and tool_progress for any in-flight call while a tool runs switches the running call, resetting timeout/liveness/elapsed. Tests for both.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented as planned. 'Live children' interpreted as runnable/disk-wait processes (state R/D/U) plus any change in the process count: a group with a sleeping child (sleep 600 = bash + sleep) is quiet, so AC1/AC2 do not conflict. Quiet threshold is max(stall_after, 6s = two 3s samples) to avoid flicker. Unknown liveness (non-bash tools, shell fallback, old helpers) never warns; shows elapsed/timeout plus a neutral 'no output for Xs' note. Linux uses /proc stat (pgrp or session match, utime+stime+cutime+cstime via sysconf CLK_TCK; nix 'feature' feature added in harness-tools); macOS/other unix one 'ps -A -o pgid=,stat=,time=' run; no new crates. Remote: helper sends ephemeral exec.liveness notes (not logged/replayed; old clients ignore unknown notes); HelperOptions.liveness_interval. New additive event tool_progress {call_id, timeout_ms?, liveness?}. Activity line is display-only text (no new interactive element); the cancel hint names the configured interrupt key.
Validation: cargo nextest run --workspace --no-fail-fast: 1736/1737 pass; the one failure is theme::tests::no_raw_colors_outside_theme flagging crates/harness-tui/src/selection/tests.rs (the other engineer's in-progress TASK-149 file, not touched here). clippy -D warnings clean for harness-tools/events/agent/remote/config/tui. Late in the session a concurrent edit added glob/grep modules to harness-tools/src/lib.rs that do not yet exist, so harness-tools does not build at this instant; that is not part of this task.

Review fixes (both blocking findings confirmed in code and fixed):
1. Approval path: ChatView::push_approval entered WaitingApproval via set_phase, so the running phase was never saved, and approvals_answered forced Sending; an approved bash call then ran in Sending (progress ignored, model-silence warning). New Activity::enter_approval (saves the interrupted phase, shared with the approval_requested event, whichever comes first) and Activity::approval_answered (resumes it, Sending only when nothing was saved; call id, timeout and liveness are kept). A ToolCall that arrives while the prompt already shows updates the saved phase instead of leaving the approval. Tests: chat::view approved_tool_resumes_running_with_progress_not_model_silence (tool_call, UI prompt, approval_requested, 'y', approval_resolved, progress samples -> 'running bash: ... 25.0s of 10m · cpu 23.9s (1 process)', no 'waiting on the model'), indicators approval_resumes_the_interrupted_tool_with_its_progress.
2. Batches: Activity now keeps the in-flight calls of the batch (ToolCall seen, no ToolResult; cleared on model_call_started/finish). tool_progress for an in-flight call other than the shown one switches the running tool to it (tool/summary, fresh elapsed, timeout, liveness). Progress for unknown or finished calls is still ignored (existing assertion unchanged). Test: progress_from_an_earlier_call_of_the_batch_makes_it_the_running_tool (busy first bash before a read: elapsed of 10m + cpu; idle first bash flagged quiet; finished call's late progress ignored).
Mutation check: reverting either fix makes the new tests fail. Residual (not changed): after a sampled bash finishes and a later non-bash call of the same batch runs, the line still names the bash call until results arrive, since only bash reports a start; such calls are normally short.
Validation: cargo nextest run -p harness-tui 400/400 pass; clippy -p harness-tui --all-targets -D warnings clean; rustfmt applied. Docs: docs/tui-views.md activity-line note on batches and approvals. The real-TUI tmux repro was not re-run; the ChatView test drives the same envelope/key path.

Integration check (both tasks together, 2026-10-23): cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1812 passed, 10 skipped; cargo test --workspace --locked --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest run -p harness -p harness-eval 213 passed, nothing written to HOME/XDG. No conflicts found between TASK-149 and TASK-150 in the TUI app. DoD #1 'in CI' is pending until the orchestrator pushes; nothing committed.

Committed in 58b1512; CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40009831788.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Activity line now distinguishes model silence from busy-but-quiet tools. harness-tools: new liveness module (Linux /proc, macOS ps) sampled by LocalExec every 3s off the async threads, ExecHandle::with_liveness/next_event, bash emits ToolEvent::Progress (timeout, samples) -> new tool_progress event. Remote helper forwards exec.liveness; shell fallback reports none. TUI: running tool shows 'elapsed of timeout · cpu X (N processes)', a sampled idle tool gets a soft warn-coloured 'bash quiet for Xs (no output, no CPU) · ctrl+c to cancel', unknown liveness falls back to elapsed/timeout, provider silence warns 'waiting on the model: no data for Xs'. Review fixes: a tool approved from the prompt resumes as the running tool (Activity::enter_approval/approval_answered) instead of falling back to Sending, and progress from any in-flight call of a multi-call batch makes it the running tool. Tests: injected-clock unit tests for each state including approval resume and batches, ChatView approval regression test, parser tests, real local busy-vs-sleep CPU test, remote helper liveness test, dispatch event test. Docs: tui-views.md, tui.md, events.md, tools.md, remote.md, doc-2/doc-7, reference ini, CHANGELOG.
<!-- SECTION:FINAL_SUMMARY:END -->
