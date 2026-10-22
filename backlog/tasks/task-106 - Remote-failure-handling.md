---
id: TASK-106
title: Remote failure handling
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:21'
labels:
  - remote
  - problem-1
milestone: m-10
dependencies:
  - TASK-102
priority: medium
type: feature
ordinal: 106000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Networks drop. Remote runs must fail cleanly or recover, never hang or leave orphans.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Disconnect mid-exec reattaches if the helper is alive, otherwise fails the step for retry
- [x] #2 Remote processes are cleaned up on cancel and failure
- [x] #3 Tests simulate disconnects
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
1. Jobs outlive connections in the helper, with sequenced output logs. 2. Reattach socket per session; stdio proxy on reconnect. 3. Client reconnect + exec.attach from last seq; Lost when the helper is gone. 4. Kill on cancel; grace-period kill when no client returns. 5. Disconnect simulation tests (in-process and real helper process).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Design:
- Jobs outlive connections. The helper keeps a per-job output log with sequence numbers, capped at 16 MiB; if bytes are dropped, the client is told with a stderr note.
- In stdio mode the helper listens on a private Unix socket <socket-dir>/helper-<session>.sock (dir 0700). A later `remote-agent --stdio --session <same>` proxies to the running helper.
- On EOF or a keepalive timeout, RemoteClient reconnects (3 attempts, backoff) and calls exec.attach{job, from_seq} for every job. If attach fails (the helper died or the network is down), the job's status sender is dropped, so ExecHandle::wait returns ExecError::Lost (the step fails and is eligible for retries), and ConnectionState::JobLost is emitted.
- Cleanup: dropping or killing the handle sends exec.kill, and the helper kills the process group (LocalExec). A helper with no client for the grace period (default 120 s, --grace) kills all job groups and exits.

Tests (tests/failures.rs, all simulating disconnects):
- disconnect_mid_exec_reattaches_without_gaps_or_duplicates.
- dead_helper_fails_the_step_and_its_processes_are_cleaned_up (ExecError::Lost, JobLost, orphaned helper kills its grandchild).
- network_down_fails_jobs_and_helper_cleans_up.
- cancel_kills_the_remote_process_group.
- helper_process_survives_disconnect_and_reattaches_through_its_socket (real helper binary; relay cut; the same helper pid answers through the socket).
- Also client::tests::keepalive_detects_a_hung_helper.

Retry is the workflow engine's job: it should treat ExecError::Lost from a remote node as retryable (harness-workflow, TASK-104 / retries).
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Disconnects mid-exec reattach to the still-running helper (Unix socket per session, sequenced output replay with no gaps or duplicates); when the helper is gone or unreachable the job fails with ExecError::Lost for retry; remote process groups are killed on cancel and by the helper's grace-period reaper on failure. Verified by tests/failures.rs (in-process and real helper binary with a cut relay). DoD#4 N/A (no TUI).
<!-- SECTION:FINAL_SUMMARY:END -->
