---
id: TASK-161.6
title: CI guard for harness overhead
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 21:08'
updated_date: '2026-10-24 00:41'
labels:
  - performance
dependencies: []
parent_task_id: TASK-161
priority: high
ordinal: 174000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
harness bench --fake measures harness overhead deterministically (~0.1 s per run, release). Add a CI step that runs it in a consistent build profile and fails when summary.overhead_per_call_ms or session start regresses past a documented threshold with headroom for runner noise (e.g. best of N runs).

Context: TASK-161 notes (baseline report, web research) and docs/performance.md; measure with harness bench (--fake for overhead, live via OpenRouter Opus 5.5, targeted and budget-capped).
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 CI runs the guard on Ubuntu and macOS without flakes; thresholds and how to update them are documented in docs/performance.md and docs/ci.md
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
1. scripts/bench-guard.sh: run harness bench --fake (dev-profile binary the test job already built, so no extra compile and a consistent profile) in a temp HOME/XDG/TMPDIR; metric = best (min) over 7 runs of summary.overhead_per_call_ms and summary.session_start_ms; retry up to 3 attempts, fail only when all are over; limits 15 ms / 50 ms (~4x dev baseline) overridable by env.
2. ci.yml test job (ubuntu + macos): step after nextest, cargo build -p harness + guard, step timeout.
3. Tests crates/harness/tests/bench_guard.rs: fake harness for pass/retry/fail/missing metric/missing binary + isolation; real binary with unreachable limits (report fields); CI step present on both OSes; script defaults == docs/performance.md table.
4. Docs: docs/performance.md 'CI overhead guard' (limits, baseline, noise handling, how to update), docs/ci.md table row + paragraph + local repro. actionlint + shellcheck.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented scripts/bench-guard.sh, CI step 'Harness overhead guard' in the test job (ubuntu-latest + macos-latest, after nextest, timeout 5 min), tests crates/harness/tests/bench_guard.rs (5 tests), docs/performance.md 'CI overhead guard' and docs/ci.md (table row, paragraph, local repro).
Profile choice: dev profile (target/debug/harness), the binary the test job already builds, so zero extra compile and the same profile every run; release would add a full LTO build per OS.
Measurements (dev profile, Linux x86_64 16 CPUs, this machine, under concurrent builds by other agents): 10 consecutive guard runs, best-of-7 per attempt: overhead per call 3.26-3.76 ms, session start 10.95-12.86 ms; all passed on attempt 1; one attempt ~1.4 s wall. Limits 15 ms / 50 ms (~4x headroom); forced-fail check (BENCH_GUARD_MAX_OVERHEAD_MS=1) fails after all attempts with exit 1. No perf change to the harness itself, so before = after for bench numbers.
Validation: cargo nextest -p harness --test bench_guard --test ci_workflow --test hermetic: 19/19 pass (bench_guard 3 extra reruns pass); actionlint 1.7.7 on ci.yml clean (release-harness.yml has a pre-existing unknown-label warning for macos-15-intel, outdated actionlint label list, not touched); shellcheck clean; clippy on the bench_guard test target shows no warnings in it (clippy -D warnings currently fails in harness-agent compaction/manager.rs:97 large_enum_variant, another engineer's in-progress edit). No live calls, $0 spend.
Remaining: the first hosted CI run on macOS/Ubuntu (no push allowed from this task) to confirm no flakes on runners; adjust limits there if runner numbers differ much (docs say how).

Review fix (relative binary path): confirmed the finding — scripts/bench-guard.sh target/debug/harness (the CI form) failed 3/3 with 'env: target/debug/harness: No such file or directory' because measure() cds into the temp dir. Fix: the guard now makes the binary path absolute against the caller's cwd before the -x check (dir/name via cd+pwd; bare name -> $PWD/name; missing dir keeps $PWD/path so the usage error names it, exit 2). New tests in crates/harness/tests/bench_guard.rs: guard_accepts_a_relative_binary_path (fake harness as <dir>/harness from the parent dir and as a bare name; missing relative path -> exit 2) and guard_runs_the_real_binary_from_a_relative_path (real binary as <profile>/harness from the target dir, the exact CI shape). Both fail against the old script (verified by temporarily reverting) and pass with the fix. Validation: nextest -p harness --test bench_guard --test ci_workflow --test hermetic 21/21 pass; the exact CI command scripts/bench-guard.sh target/debug/harness from the repo root now passes (overhead 3.18 ms, session start 10.95 ms); shellcheck clean; rustfmt clean. AC #1 unchecked: 'without flakes on Ubuntu and macOS' needs a hosted CI run, which this task cannot do (no push); DoD #1 stays unchecked for the same reason. Remaining: first hosted CI run on both OSes, then check AC #1/DoD #1. No live calls, $0.

Hosted CI https://github.com/thowd22/harness/actions/runs/40045278391 (commit 63eb3bf), all jobs green. Guard on the runners: ubuntu-latest overhead 3.988 ms (max 15), session start 11.756 ms (max 50); macos-latest 3.331 ms, 13.712 ms; both passed on attempt 1/3. That is close to local numbers (3.2 ms, 11 ms), so the limits are unchanged. Before the push the jq dependency was removed (POSIX tr/sed parse), because 6 bench_guard tests failed on a stock Ubuntu 24.04 container without jq. After the fix the stock container passed 2043/2043.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
scripts/bench-guard.sh runs harness bench --fake in an isolated temp HOME/XDG, takes the best of 7 runs per attempt, and fails only if all 3 attempts exceed 15 ms overhead per call or 50 ms session start. It runs in the CI test job on Ubuntu and macOS and needs only POSIX tools. Verified by 7 bench_guard integration tests, a stock Ubuntu 24.04 container run and hosted CI https://github.com/thowd22/harness/actions/runs/40045278391 (ubuntu 3.99/11.76 ms, macOS 3.33/13.71 ms, attempt 1/3).
<!-- SECTION:FINAL_SUMMARY:END -->
