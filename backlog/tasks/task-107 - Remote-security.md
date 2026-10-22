---
id: TASK-107
title: Remote security
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:21'
labels:
  - remote
  - security
  - problem-8
milestone: m-10
dependencies:
  - TASK-102
  - TASK-25
priority: high
type: feature
ordinal: 107000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Extending agents to other machines widens the blast radius.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 No agent forwarding or env forwarding; remote exec env is scrubbed
- [x] #2 Secrets are never sent to remote hosts, verified by tests on the wire protocol
- [x] #3 Per-host policy can only tighten global policy
- [x] #4 Every remote operation emits an audit event
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
1. ssh never forwards agent/X11/ports/env. 2. Remote exec env scrubbed; local host vars never forwarded. 3. SecretGuard on every outgoing frame and raw payload; wire-tap tests. 4. Per-host policy as tighten-only PolicyOverlay. 5. AuditEvent for every remote op (RPC, sync push, shell scripts).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
- AC#1: SshTarget always passes ForwardAgent=no, -a, ForwardX11=no, -x, ClearAllForwardings=yes, SendEnv=-* and PermitLocalCommand=no (ssh::tests::no_forwarding_of_agent_env_or_ports). RemoteEnv scrubs remote exec env with EnvScrubber: the default names, HARNESS_*, configured names, and values containing a key. Local host-specific vars (HOME, PATH, SSH_*, XDG_*, DISPLAY, ...) are never forwarded; the helper substitutes remote login values only for HOME/PATH/USER/LOGNAME/SHELL/TMPDIR/LANG/LC_* (workspace::tests, tools_remote::secrets_and_local_env_never_reach_the_remote_host).
- AC#2: SecretGuard (ClientOptions::with_secrets) checks every outgoing request and every raw file payload before base64. Writes and commands that contain a credential are refused. tools_remote::secrets_and_local_env_never_reach_the_remote_host taps the wire bytes and asserts the secret and its base64 are absent, as are the local HOME and agent socket. client::tests::secrets_are_blocked_before_sending also covers this.
- AC#3: RemoteHost::policy_overlay() is a harness_policy PolicyOverlay (deny/ask only) applied with PolicyEngine::with_overlay. hosts::tests::per_host_policy_only_tightens checks that the host is at least as strict as global for every command, that host ask beats a global allow, that global denies still apply, and that bash.allow in an overlay is rejected.
- AC#4: every RPC produces an AuditEvent{host, op, target, ok, error, duration, at} through ClientOptions.audit (default TracingAudit, logged to target harness::remote::audit). sync.push (rsync/bundle) is audited via RemoteClient::record_audit, and shell scripts (install, upload, fallback) via shell::AuditedShell. Tests: tools_remote::every_remote_operation_is_audited_with_its_host, sync::push_is_audited_with_the_host, shell::tests::audited_shell_records_every_script.

Integration still needed in other crates:
- The binary/agent must build ClientOptions::new(name).with_secrets(resolved credentials).with_audit(..) and RemoteEnv::new(EnvScrubber::new(cfg.secret_env_vars(), secrets)) for each remote host.
- To also put audit events in the run event log, harness-events needs an EventKind for remote ops and harness-agent an AuditSink over RunRecorder (see the TASK-102 notes).
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Remote security: no agent/X11/port/env forwarding, scrubbed remote exec env with no local host variables, a secret guard that refuses any outgoing frame or payload containing a credential (verified on tapped wire bytes), tighten-only per-host policy overlays, and host-tagged audit events for every RPC, sync push and shell script. Verified by tests in tools_remote.rs, hosts.rs, ssh.rs, client.rs, sync.rs and shell.rs. DoD#4 N/A (no TUI).
<!-- SECTION:FINAL_SUMMARY:END -->
