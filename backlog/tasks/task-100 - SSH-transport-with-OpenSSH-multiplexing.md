---
id: TASK-100
title: SSH transport with OpenSSH multiplexing
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:19'
labels:
  - remote
  - spike
milestone: m-10
dependencies:
  - TASK-99
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: high
type: feature
ordinal: 100000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
System ssh respects the user's ssh config, agent and jump hosts. A spike confirms this over a pure-Rust client.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Spike documents OpenSSH versus russh decision
- [x] #2 ControlMaster multiplexing with ControlPath in the runtime dir
- [x] #3 Strict host key checking; unknown hosts fail with a clear message
- [x] #4 Keepalive, reconnect and connection lifecycle events
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
1. Spike: OpenSSH vs russh, documented in docs/remote.md and doc-6. 2. ssh::SshTarget builds every invocation with ControlMaster=auto, ControlPath under $XDG_RUNTIME_DIR/harness (0700), StrictHostKeyChecking=yes, BatchMode, keepalives, no forwarding. 3. classify() turns ssh stderr into actionable SshError (unknown host, changed key, auth, resolve, unreachable). 4. client::RemoteClient: keepalive ping, reconnect with backoff, ConnectionEvent lifecycle broadcast.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Decision spike: system OpenSSH over russh. The comparison table is in docs/remote.md ("Decision: OpenSSH instead of a Rust SSH client") and is summarised in doc-6 "Decisions".

ssh.rs: SshOptions, SshTarget::{option_args, command_args, command, command_line, rsync_shell, prepare, master_running, close_master, run}, plus shell_quote, quote_remote_path, remote_command_line, classify -> SshError, control_dir_with and create_private_dir.

client.rs: the Connector trait (SshConnector, CommandConnector, InProcessConnector) and RemoteClient. RemoteClient sends a keepalive ping every 15 s (a timeout means Disconnected), reconnects up to 3 times with backoff, and broadcasts ConnectionEvent{host, state: Connecting | Connected{reconnected, reattached} | Disconnected | Reconnecting{attempt, max} | Failed | JobLost}.

Tests:
- ssh::tests: multiplexing, strict host keys, keepalive options, no forwarding, and the unknown-host message, including a fake ssh binary that exits 255.
- client::tests::{lifecycle_events_on_disconnect_and_reconnect, keepalive_detects_a_hung_helper}.
- The real-sshd test is #[ignore].

The lifecycle events are a broadcast inside harness-remote. Mapping them into harness_events run events is a cross-crate follow-up (see the TASK-102 notes).
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
System OpenSSH transport (decision documented in docs/remote.md + doc-6): ControlMaster multiplexing with a 0700 ControlPath dir under XDG_RUNTIME_DIR, strict host keys with clear unknown/changed-key messages, keepalives, reconnect and host-tagged connection lifecycle events. Verified by unit tests (arg construction, stderr classification, fake ssh) and client tests (disconnect/reconnect events, hung-helper keepalive). DoD#4 N/A (no TUI).
<!-- SECTION:FINAL_SUMMARY:END -->
