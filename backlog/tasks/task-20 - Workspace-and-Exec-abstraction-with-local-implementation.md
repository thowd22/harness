---
id: TASK-20
title: Workspace and Exec abstraction with local implementation
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:11'
labels:
  - tools
  - core
milestone: m-2
dependencies:
  - TASK-1
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: high
type: feature
ordinal: 20000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
All file and process operations go through one trait so tools work unchanged on local and remote (SSH) hosts, and so policy and change tracking have a single interception point.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Workspace trait covers read, write, atomic replace, stat, list and glob
- [x] #2 Exec trait covers spawn with streaming output, timeout, kill of the whole process group and exit status
- [x] #3 Local implementation passes a shared trait test suite that remote implementations will also run
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
1. Add deps (tokio, async-trait, globset, ignore, nix for killpg). 2. workspace.rs: async Workspace trait (resolve, read, write, replace_atomic, stat, list, glob) + WsError. 3. exec.rs: Exec trait, ExecRequest, ExecHandle (channel-backed so remote impls can build one), OutputChunk, ExitOutcome. 4. local.rs: LocalWorkspace (tempfile+rename preserving perms, symlink-aware) and LocalExec (process_group(0), stdin null, supervisor task: timeout/kill/drop -> SIGTERM then SIGKILL to the group, stragglers killed after leader exit). 5. conformance.rs: public shared suite run against Local; remote impls reuse it. 6. docs/tools.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Workspace (read/write/replace_atomic/set_mode/stat/list/glob/resolve) and Exec (spawn -> channel-backed ExecHandle with streaming chunks, timeout, kill/drop kills the process group, ExitOutcome) implemented; LocalWorkspace/LocalExec pass harness_tools::conformance::{workspace_suite, exec_suite}. Added nix (signal, process) to workspace deps for safe killpg.

Validation: cargo test -p harness-tools (50 unit + 1 doctest pass), cargo clippy -p harness-tools --all-targets -- -D warnings clean, cargo doc warning-free. DoD#4 N/A (no TUI). Docs: docs/tools.md (For implementers).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added harness_tools::workspace::Workspace (resolve/read/write/replace_atomic/set_mode/stat/list/glob, WsError) and harness_tools::exec::{Exec, ExecRequest, ExecHandle, ExitOutcome}. LocalWorkspace (tempfile+rename, keeps mode, follows symlinks, gitignore-aware glob) and LocalExec (stdin null, own process group, SIGTERM->SIGKILL of the group on timeout/kill/drop, stragglers killed after leader exit). Shared suite harness_tools::conformance::{workspace_suite, exec_suite} passes for Local; remote impls must run it too.
<!-- SECTION:FINAL_SUMMARY:END -->
