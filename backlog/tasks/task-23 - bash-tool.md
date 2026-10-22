---
id: TASK-23
title: bash tool
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:11'
labels:
  - tools
  - problem-1
  - problem-8
milestone: m-2
dependencies:
  - TASK-20
  - TASK-5
priority: high
type: feature
ordinal: 23000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Shell execution is where hangs, runaway processes, giant outputs and credential leaks occur.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Commands run non-interactively with stdin closed and a default and maximum timeout
- [x] #2 Timeout or cancel kills the entire process group
- [x] #3 Output streams live to the UI and is truncated for the model with head and tail plus a spill pointer
- [x] #4 Subprocess environment is scrubbed of harness secrets and provider keys
- [x] #5 Working directory persists across calls within a session
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
1. BashTool per session over Arc<dyn Exec>: bash --noprofile --norc with a wrapper script (cd to session dir, eval command, EXIT trap prints a nonce marker + PWD). 2. Stream chunks as ToolEvent::Output, strip marker with a streaming filter, capture bounded head/tail. 3. Default/max timeout from ToolsConfig; timeout/cancel(drop) kill the group via Exec. 4. EnvScrubber (default provider key names, HARNESS_*, configured names, secret values) + non-interactive env. 5. Spiller for head/tail + spill pointer. 6. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Validation: cargo test -p harness-tools (50 unit + 1 doctest pass), cargo clippy -p harness-tools --all-targets -- -D warnings clean, cargo doc warning-free. Env scrubbing takes secret names/values as input (EnvScrubber::new) instead of depending on TASK-5; the binary/agent must pass the api_key_env names and resolved key values from harness-config. DoD#4 N/A.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
harness_tools::bash::BashTool: stdin closed, default 120s / max 600s timeouts, whole process group killed on timeout or when the call future is dropped, live output events, head+tail truncation with spill:<id> pointer, scrubbed env (EnvScrubber), persistent cwd. Verified by bash::tests (timeout and cancel tests check the grandchild is dead).
<!-- SECTION:FINAL_SUMMARY:END -->
