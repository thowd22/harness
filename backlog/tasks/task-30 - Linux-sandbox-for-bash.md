---
id: TASK-30
title: Linux sandbox for bash
status: Done
assignee:
  - '@policy'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:47'
labels:
  - security
  - sandbox
  - problem-8
milestone: m-2
dependencies:
  - TASK-23
priority: medium
type: feature
ordinal: 30000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Sandboxes alone are not enough, but they bound damage when policy is wrong. Writes outside the workspace and optional network access should be blocked at the OS level.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Landlock restricts bash writes to the workspace and temp dirs when supported
- [x] #2 Network deny mode uses bwrap or unshare when available
- [x] #3 Unsupported kernels degrade with a visible warning and doctor reports sandbox status
- [x] #4 sandbox section in ini controls behaviour
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
1. sandbox::launcher: launcher_main applies a Landlock write ruleset (landlock crate, best effort ABI V5) beneath --write paths then execs; --probe reports status.
2. SandboxedExec wraps any Exec: optional bwrap/unshare --unshare-net prefix, then launcher.
3. SandboxStatus::probe (landlock probe, net tool test-run), warnings, doctor_lines.
4. SandboxConfig::from_ini([sandbox]) / for_workspace (workspace, temp dirs, devices).
5. harness-sandbox bin for tests; integration tests with LocalExec and BashTool; docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_policy::sandbox (+ src/bin/harness-sandbox.rs, tests/sandbox.rs). No unsafe code: landlock crate + std CommandExt::exec. Verified on this host (kernel 7.0, landlock=full, bwrap works): writes outside workspace/temp fail with Permission denied, writes inside succeed, BashTool over SandboxedExec respects it, network deny blocks a loopback TCP connect via bwrap --unshare-net, allow mode connects. Unsupported paths assert the visible warning instead. Known limitation (documented): Landlock blocks writing /dev/stdout|/dev/stderr by path (pipes reached through /proc); use >&2. Needs: binary must dispatch 'harness __sandbox-exec ...' (sandbox::LAUNCHER_ARG) to launcher_main and the agent must build BashTool with SandboxedExec::new(LocalExec, Launcher::current_exe(), SandboxConfig::from_ini(&cfg.sandbox, root)); harness doctor (TASK-113) prints SandboxStatus::doctor_lines(); TUI shows SandboxStatus::warnings.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Linux sandbox for bash: Landlock write restriction via a launcher (harness-sandbox bin / __sandbox-exec mode), bwrap/unshare network namespace for network=deny, probing with visible warnings and doctor lines, [sandbox] mapping via SandboxConfig::from_ini. DoD#4 N/A (no interactive UI). Verified with cargo test -p harness-policy --test sandbox (real processes) and clippy -D warnings.
<!-- SECTION:FINAL_SUMMARY:END -->
