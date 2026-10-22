---
id: TASK-129
title: Per-host SSH compression and zstd rsync
status: Done
assignee: []
created_date: '2026-10-22 19:32'
updated_date: '2026-10-22 21:16'
labels:
  - remote
  - config
  - problem-6
milestone: m-10
dependencies:
  - TASK-99
  - TASK-100
  - TASK-103
  - TASK-105
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: medium
type: feature
ordinal: 129000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Remote tool traffic and workspace sync can be large on slow links. OpenSSH already offers zlib channel compression and modern rsync supports zstd, so we can get gains without protocol changes. Compression hurts on fast LANs, so it must be configurable per host.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 remote.name compression = auto, yes or no in harness.ini; yes adds Compression=yes to the ssh options for that host's ControlMaster
- [x] #2 auto enables SSH compression only when the measured link throughput from the remote probe is below a configurable threshold
- [x] #3 rsync uses --compress-choice=zstd when both ends support it and falls back to its default compression otherwise
- [x] #4 remote test and the remotes view show whether compression is active for each host
- [x] #5 Tests cover the generated ssh and rsync arguments for each setting
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
1. Config: [remote.<name>] compression (auto|yes|no) + compression_below_mbps. 2. harness-remote link module: throughput measurement over RemoteShell, persisted LinkStore, decide(), rsync zstd detection. 3. SshTarget compression flag: Compression=yes/no + separate cmz-%C ControlPath. 4. rsync_args takes RsyncCompression; rsync_push detects both ends. 5. SshHosts decides per host (measures auto hosts with no fresh sample), audits link.compression. 6. /remote list and test report show compression; harness wiring persists measurements in data dir. 7. Docs + changelog.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Decisions: a mux client cannot change the master's compression, so compressed connections use their own ControlPath (cmz-%C). Off states pass Compression=no explicitly so the displayed state is accurate (recorded under CHANGELOG Defaults). Link measurement = time 'true' then 'head -c 256KiB /dev/urandom' over ssh, difference is the transfer; random bytes so compression does not inflate the figure; 10s timeout records an upper bound (always slow). Measurements persist in <data_dir>/remote-links.json (LinkStore), trusted 24h; SshHosts measures on connect only for auto hosts without a fresh sample. Shared-file edits: crates/harness/src/tui.rs (Driver::new uses crate::remote::probe_cache so /remote reads persisted measurements; 2 lines), Remote struct literal in crates/harness-tui/src/palette.rs and harness-config agents/tests.rs (2 added fields each). Not verified against a real SSH host (no host available); all tests offline (LocalShell, in-process helper). Validation: cargo test --workspace green; clippy -D warnings clean for harness-remote, harness-config, harness, harness-tui, harness-workflow; cargo fmt --check clean.

Review fixes (link measurement): (1) measure_link now runs an untimed 'true' first to bring up the ControlMaster (TCP connect, kex, auth), then times a second 'true' on the warm mux as overhead, then times the 256 KiB sample, so the SSH handshake is no longer subtracted from the transfer. Uses tokio::time::Instant so tests run under paused time. Test the_ssh_handshake_does_not_inflate_the_throughput reproduces the reviewer's 10 Mbit/s / 50 ms RTT example (300 ms handshake, 100 ms overhead, 310 ms sample) and asserts 9.99 Mbit/s and compression on; the old ordering would have given 210 Mbit/s. (2) Connection setup and the overhead round trip each have their own timeout and a timeout or failure there is Err (measure_host_link then stores nothing, so auto stays unmeasured/uncompressed or keeps the previous sample); only a transfer that does not finish within MEASURE_TIMEOUT records the upper bound. a_measurement_that_times_out_records_an_upper_bound now uses a shell whose transfer (not first call) hangs, same assertions; new tests a_connection_setup_that_hangs_is_an_error_and_stores_nothing and a_failing_connection_setup_is_an_error. docs/remote.md updated. Added tokio test-util to harness-remote dev-deps. Remaining gap: the SshHosts::compression connect-time path still has no direct test (it builds a real SshShell internally); it calls the same measure_host_link covered above. Still not verified against a real SSH host. Validation: cargo test -p harness-remote 83 passed / 1 ignored (pre-existing), clippy -p harness-remote --all-targets -D warnings clean, cargo fmt applied, cargo check -p harness OK.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added per-host SSH compression ([remote.<name>] compression = auto|yes|no, compression_below_mbps default 20) and zstd rsync. yes/no set Compression=yes/no on the host's ControlMaster (compressed masters on a separate cmz-%C socket); auto compresses only when the probe's measured link throughput (harness_remote::link::measure_link, persisted LinkStore) is below the threshold. rsync adds --compress-choice=zstd when both rsync --version outputs list zstd, else plain --compress. /remote list lines and the test report show compression state, link speed and rsync compression. Tests: link::tests (decide per setting, threshold, measurement, timeout, store, rsync detection), ssh::tests compression_* / auto_decision_sets_compression, sync::tests rsync_args_per_compression_setting, hosts::tests list_lines_show_compression_per_host / test_report_shows_link_and_compression, config remote_compression_settings. DoD UI: N/A (text in the existing /remote panel, no new interactive elements). Not live-verified against a real SSH host.
<!-- SECTION:FINAL_SUMMARY:END -->
