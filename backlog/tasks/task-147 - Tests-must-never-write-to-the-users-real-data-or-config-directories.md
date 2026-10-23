---
id: TASK-147
title: Tests must never write to the user's real data or config directories
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 06:44'
updated_date: '2026-10-23 09:29'
labels:
  - testing
  - eval
  - security
  - problem-8
milestone: m-11
dependencies: []
priority: high
type: bug
ordinal: 151000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Running the test suite on a developer machine left an eval session (/tmp/harness-eval-long-context-…, 'In data/settings.txt…') in the user's real ~/.local/share/harness sessions list: some harness-eval tests run the real binary through the process executor without isolating its data dir, HOME or XDG paths. Tests must be hermetic so they cannot pollute or read a user's sessions, memory, config or credentials.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Every test that runs the harness binary or creates a data dir points HARNESS_CONFIG, XDG_CONFIG_HOME, XDG_DATA_HOME, XDG_CACHE_HOME and HOME at a temp dir
- [x] #2 The eval process executor isolates each attempt's data dir by default, and documents how to opt in to a shared one
- [x] #3 A guard test runs the full harness-eval and harness integration suites with HOME and XDG dirs pointed at a sentinel directory and fails if anything is created there
- [x] #4 The leftover eval session in the user's real sessions list is explained in the notes (it was created by a local test run), and the CHANGELOG notes the fix
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
1. Add HARNESS_DATA_DIR env override of [general] data_dir (harness-config paths/load).
2. ProcessExecutor: fresh temp data dir (HARNESS_DATA_DIR) + XDG_CACHE_HOME per attempt by default; with_shared_data_dir / harness eval run --shared-data-dir opt-in.
3. Tests running the binary: common::hermetic helper (HARNESS_CONFIG, HOME, XDG_CONFIG/DATA/CACHE/STATE_HOME; drops HARNESS_DATA_DIR/HARNESS_LOG) used by Env::cmd, release.rs, remote_agent.rs; ignored starter binary test gets a hermetic env.
4. scripts/hermetic-guard.sh runs harness + harness-eval suites with HOME/XDG at a sentinel and fails on any created file; CI step; tests in crates/harness/tests/hermetic.rs (fake cargo, CI wiring, static check of binary spawns).
5. Docs (data-dir, evals, ci, doc-2) and CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Leftover session /tmp/harness-eval-long-context-… ('In data/settings.txt…') in the real ~/.local/share/harness sessions list: created by a local run of the starter suite through the real binary (the #[ignore]d starter_suite_through_the_harness_binary test, run with HARNESS_BIN, or a manual 'harness eval run' of suites/starter) before this fix. ProcessExecutor spawned 'harness run' with the caller's full environment and no data-dir isolation, so 'harness run' saved its session in the user's data dir. The long-context task's workspace is a /tmp/harness-eval-long-context-* temp dir and its prompt starts 'In data/settings.txt', matching the entry. Not deleted (user's real data is left untouched); the user can remove it from the sessions list themselves.
Baseline guard run (before fixes) found one more offender: tui::tests::remote_lists_hosts_and_tests_them_with_health_badges creates $XDG_RUNTIME_DIR/harness (ssh ControlMaster socket dir via SshOptions::default()). That is per-boot socket state, not user config/data, so the guard redirects XDG_RUNTIME_DIR to a sentinel but does not count it as a leak.
Validation: scripts/hermetic-guard.sh nextest run --no-fail-fast -p harness -p harness-eval -p harness-config -> 342/343 pass, nothing written to HOME/XDG. The one failure (harness-eval::defaults_changelog fingerprint) comes from concurrent [mcp] defaults work in harness-config/src/ini/config.rs+mcp.rs by another engineer (files modified after this task started; passed in the baseline run), not from this task. Ignored binary test verified hermetic: HARNESS_BIN=$PWD/target/debug/harness scripts/hermetic-guard.sh test -p harness-eval --test starter -- --ignored -> ok. clippy -D warnings clean for harness, harness-eval, harness-config.

Integration check: the harness-eval defaults_changelog failure noted at handoff is resolved. The [mcp] defaults entry in CHANGELOG Defaults now matches, and the test passes. The full workspace suite and the hermetic guard (harness, harness-eval, harness-mcp) pass locally. The CI guard step and macOS still wait for the orchestrator's push.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Tests and evals are hermetic. New HARNESS_DATA_DIR env overrides [general] data_dir; the eval ProcessExecutor gives every attempt a temp data+cache dir (opt-in to the configured one with --shared-data-dir / with_shared_data_dir). All tests running the harness binary use common::hermetic (HARNESS_CONFIG, HOME, XDG_* at a temp dir); the ignored starter binary test passes a hermetic env. scripts/hermetic-guard.sh reruns the harness and harness-eval suites with HOME/XDG at a sentinel and fails on any created file; CI runs it. Tests: exec.rs (per-attempt isolation, opt-in, env precedence), cli.rs (--shared-data-dir), paths/load (override), crates/harness/tests/hermetic.rs (guard with fake cargo, CI wiring, every binary spawn isolated). Docs: data-dir, evals, ci, doc-2; CHANGELOG Fixed. DoD #4 N/A (no UI).
<!-- SECTION:FINAL_SUMMARY:END -->
