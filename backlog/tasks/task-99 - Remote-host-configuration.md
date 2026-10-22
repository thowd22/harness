---
id: TASK-99
title: Remote host configuration
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - remote
  - config
  - problem-8
milestone: m-10
dependencies:
  - TASK-2
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: high
type: feature
ordinal: 99000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Remote boxes are declared by the user in harness.ini; project YAML must not be able to add hosts.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 remote sections support host, user, port, identity_file, labels, workdir, max_jobs, sync and tighter per-host policy
- [x] #2 /remote lists hosts and a test action checks connectivity and helper status
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
1. harness_remote::hosts::HostRegistry built only from harness.ini [remote.*] (parsed by harness-config). 2. RemoteHost: ssh target, workdir default, tighten-only PolicyOverlay, labels. 3. /remote listing lines with health badges and a test action (connect, helper version, probe). 4. Tests + docs/remote.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented crates/harness-remote/src/hosts.rs: HostRegistry::from_config (the only constructor, so project YAML cannot add hosts), get() with a declared-host hint, with_labels(), list_lines(&ProbeCache), RemoteHost::{ssh_target, workdir, policy_overlay, policy_origin, describe}, and test_host(host, connect result, cache) -> HostTestReport{connectivity, helper status, probe, health}.lines(). Tests: hosts_come_from_ini_with_all_keys, per_host_policy_only_tightens, list_lines_show_health, test_action_reports_connectivity_helper_and_probe. Docs: docs/remote.md (Declaring hosts, /remote). No new ini keys.

REMAINING (AC#2, DoD#1, DoD#4): the TUI `/remote` slash command does not exist yet. The harness-tui / slash-registry owner has to add `/remote`: render HostRegistry::list_lines, and give each host a focusable, clickable "test" action that calls hosts::test_host (mouse and keyboard, with tests for both). Then check AC#2, DoD#1 and DoD#4.

cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.

Integration: /remote lists hosts with a clickable/focusable test action per host (remote::test via connect_with_upgrade, 20s timeout) and health badges. Test: tui remote_lists_hosts_and_tests_them_with_health_badges; panel tests cover mouse and keyboard.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: /remote lists hosts with a clickable/focusable test action per host (remote::test via connect_with_upgrade, 20s timeout) and health badges. Test: tui remote_lists_hosts_and_tests_them_with_health_badges; panel tests cover mouse and keyboard. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
