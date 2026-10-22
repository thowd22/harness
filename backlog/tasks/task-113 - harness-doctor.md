---
id: TASK-113
title: harness doctor
status: Done
assignee:
  - '@memory-evals-quality'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - cli
  - problem-9
milestone: m-11
dependencies:
  - TASK-3
  - TASK-30
  - TASK-100
  - TASK-47
priority: medium
type: feature
ordinal: 113000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need one command to diagnose configuration and environment problems.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Checks config validity, credential file permissions, provider connectivity, sandbox support, remote reachability, prompt lint and shadowed definitions
- [x] #2 Prints actionable fixes
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
harness_eval::doctor: checks config validity, credential file permissions, key resolution + provider TCP connectivity, sandbox probe, ssh reachability, prompt lint and shadowed definitions; each finding has a fix; network/process access behind the Probes trait (LiveProbes for real use).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Done: harness_eval::doctor::{diagnose, run, Input, Probes, LiveProbes, Report, Check, Status, host_port}. Tests: doctor::tests (problems with fixes: insecure mode, missing key, sandbox unsupported, remote down; healthy setup exits 0; invalid ini and blanket-prohibition lint + shadowing; live TCP probe against a local listener). Docs: docs/doctor.md.
Remaining (DoD#1): the binary must add 'harness doctor [--config PATH]' calling harness_eval::doctor::run(&Input{config, project: Some(cwd), global_dir: None}, &LiveProbes::default(), &mut stdout) and exit with the code, plus a binary test. LiveProbes::default uses Launcher::current_exe(), so the binary's __sandbox-exec dispatch serves the probe. This also covers TASK-47's doctor lines.

Integration: `harness doctor` in the binary. Test: commands.rs doctor_checks_every_area_without_the_network.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: `harness doctor` in the binary. Test: commands.rs doctor_checks_every_area_without_the_network. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
