---
id: TASK-105
title: Remote resource probe and health
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 19:16'
labels:
  - remote
  - visibility
milestone: m-10
dependencies:
  - TASK-101
priority: medium
type: feature
ordinal: 105000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Placement and visibility need to know what each box offers and whether it is healthy.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Probe reports cpu, memory, gpu via nvidia-smi, disk and current jobs, cached with TTL
- [x] #2 /remote and map host badges show health
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
1. Helper probe RPC: cpu, load, memory, nvidia-smi GPUs, df disk, running jobs. 2. ProbeCache with TTL. 3. Health assessment + badge/theme token for /remote and map. 4. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
probe.rs:
- ProbeReport{helper_version, arch, os, cpus, load, mem_total/available, gpus: Vec<Gpu> from nvidia-smi csv, disk from df -Pk, jobs}.
- collect() runs inside the helper (the probe RPC). The parsers are parse_meminfo, parse_loadavg, parse_nvidia_smi and parse_df.
- ProbeCache has a TTL (default 30 s): get, put, get_or_probe, invalidate, health.
- Health{Unknown, Healthy, Degraded{reasons}, Unreachable{reason}} with assess(result, max_jobs) (low disk, low memory, overloaded, busy), badge() (ok/degraded/down/?), token() (theme token: ok/warn/error/muted) and describe().
- hosts::HostRegistry::list_lines shows the badge per host; hosts::test_host refreshes the cache.

Tests: probe::tests (parsers, collect on this machine, health, TTL expiry, get_or_probe caching), plus hosts::tests::list_lines_show_health and test_action_reports_connectivity_helper_and_probe.

REMAINING (AC#2, DoD#1, DoD#4): the TUI has no /remote view and no map host badges yet.
- harness-tui (/remote and agent map owners) should render Health::badge() with theme token Health::token() next to each host and on map nodes whose NodeStarted.host is a remote, reading ProbeCache::health(host, max_jobs). Mouse and keyboard tests are needed.
- The agent/scheduler should keep the cache fresh via RemoteClient::probe and ProbeCache::put, and invalidate it on ConnectionState::Disconnected.
Then check AC#2, DoD#1 and DoD#4.
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.

Integration: /remote shows probe health per host and map host badges (CoreEvent::HostBadge). Test: tui remote_lists_hosts_and_tests_them_with_health_badges; app host badge tests.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Integration: /remote shows probe health per host and map host badges (CoreEvent::HostBadge). Test: tui remote_lists_hosts_and_tests_them_with_health_badges; app host badge tests. Workspace green: cargo fmt --check, clippy -D warnings, cargo test --workspace (1142 passed, 0 failed).
<!-- SECTION:FINAL_SUMMARY:END -->
