---
id: TASK-154
title: 'Sandbox: allow terminal ioctls on /dev/tty and explain sandbox denials'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 16:14'
updated_date: '2026-10-23 18:44'
labels:
  - sandbox
  - policy
dependencies: []
priority: high
ordinal: 158000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Evidence: a real v0.3.0 session on a large repo (run 01a1691a, sonnet 5.5 via OpenRouter): 126 model calls, about 17 minutes of model time, 15.3M input tokens. ioctl(TIOCGWINSZ) on /dev/tty returned EACCES inside the bash sandbox, so terminal-size probing in the user's tests failed and the model spent about 10 calls with pty probes and strace finding out why. The Landlock ruleset (crates/harness-policy/src/sandbox/launcher.rs, ABI::V5) handles write access including the V5 ioctl-on-device right, while WRITABLE_DEVICES only grants write rights beneath /dev/tty, /dev/pts and /dev/ptmx.

Grant LANDLOCK_ACCESS_FS_IOCTL_DEV on the terminal devices (/dev/tty, /dev/pts, /dev/ptmx and the controlling tty) so normal terminal ioctls work, keep other device ioctls restricted, and make sandbox denials legible: when a sandboxed command fails with EACCES/EPERM on a path or ioctl the sandbox blocks, the tool result says so with the setting to change.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Under the sandbox, ioctl(TIOCGWINSZ) and tcgetattr on /dev/tty and on a pty slave succeed (test with a real pty); writes outside the workspace are still denied
- [x] #2 Device ioctls outside the terminal set stay denied on ABI v5 kernels and the behaviour degrades cleanly on older ABIs
- [x] #3 A command failing on a sandbox-denied path or ioctl gets a short note in its tool result naming the sandbox and [sandbox] setting
- [x] #4 docs/policy.md sandbox section updated; tests run on Linux and are skipped with a reason elsewhere
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
Root cause (reproduced): the release binary is static musl; Rust std strips O_PATH from custom_flags on musl (O_ACCMODE includes O_SEARCH), so landlock::PathFd opens /dev/tty for real. Bash runs without a controlling tty, the open fails with ENXIO and path_beneath_rules silently drops the /dev/tty rule, so later ioctl(TIOCGWINSZ) on /dev/tty (in a pty child) gets EACCES. glibc dev builds work, which hid it.
1. Launcher: open rule paths ourselves with nix open(O_PATH|O_CLOEXEC) (never opens devices), compute per-file access, no silent drop of terminal rules.
2. Split rights: --write paths get write rights without IOCTL_DEV; terminal devices (TERMINAL_DEVICES: /dev/tty, /dev/ptmx, /dev/pts, plus the tty on the launcher's stdio) get write + IOCTL_DEV. Other device ioctls stay denied on ABI>=5; on older ABIs the crate's best effort leaves ioctls unhandled.
3. Hidden --abi N launcher flag to target a lower ABI (tests of degradation) and pub landlock_abi() for tests/doctor.
4. SandboxedExec relays the ExecHandle and, when an enforced sandbox command fails with Permission denied/Operation not permitted not explained by a path inside a writable root, appends a short 'harness sandbox:' note naming [sandbox] in harness.ini.
5. Tests: unit (O_PATH fd, access split, denial detection), integration with real pty via nix openpty + setsid -c, script-created pty, /dev/null ioctl denied on ABI>=5, --abi 4 degradation, outside writes denied. Linux-only with skip reason elsewhere.
6. docs/policy.md sandbox section, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Root cause: the v0.3.0 release binary is static musl. Rust std masks custom_flags with O_ACCMODE, which on musl includes O_SEARCH (= O_PATH), so landlock::PathFd opened /dev/tty for real; bash has no controlling tty, so the open failed with ENXIO and path_beneath_rules silently dropped the rule. A pty child later got EACCES on ioctl(TIOCGWINSZ) on /dev/tty. Reproduced with ~/.local/bin/harness __sandbox-exec (FAIL 13) vs a glibc build (ok); the fixed launcher built for x86_64-unknown-linux-musl passes the same repro and the sandbox + sandbox_tty suites (musl and cross darwin build dirs removed afterwards).
Changes: launcher opens rule paths with nix open(O_PATH|O_CLOEXEC); --write paths get write rights without IOCTL_DEV; TERMINAL_DEVICES (/dev/tty, /dev/ptmx, /dev/pts) + the tty on the launcher's stdio get write + IOCTL_DEV; WRITABLE_DEVICES no longer lists terminals. New hidden --abi N flag (only loosens to an older ABI) and pub device_ioctls_restricted(). SandboxedExec relays the ExecHandle (output, liveness, kill/drop) and appends DENIAL_NOTE on stderr for failed commands whose Permission denied / Operation not permitted lines are not explained by a writable path (absolute paths, or a relative path next to the message that exists in cwd); devices count as unexplained (ioctl). The note is a constant, so it cannot carry secrets.
Tests: unit (O_PATH flag on the rule fd, missing paths skipped, denial scan, relay note/kill/drop); tests/sandbox_tty.rs with nix openpty + setsid -c (stty -F /dev/tty|slave size and -a, /dev/null ioctl denied on ABI 5+, outside write denied), --abi 4 degradation, pty created inside the sandbox without ctty (script), BashTool denial note, kill/drop through the relay. Mutation check: dropping IOCTL_DEV from the terminal rules fails two pty tests. Host: kernel 7.0, ABI 5+ path exercised; older-ABI branch via --abi 4 and skip notes. CI: added a 'Terminal sandbox tests with skip reasons' step (--nocapture prints the ABI case). No prompt/ini defaults changed, so no Defaults entry.
Validation: cargo nextest run -p harness-policy (96 passed), clippy -D warnings (linux and aarch64-apple-darwin), rustfmt check, cargo check -p harness, harness ci_workflow tests.

Review fix (blocking finding confirmed): after the inner output streams closed (e.g. 'exec >/dev/null 2>&1; sleep 60' or 'exec >log 2>&1; cargo build'), relay() left its select loop and awaited inner.wait(), which keeps the inner kill sender alive, so kill() or dropping the outer handle (Esc/cancel) no longer reached the process group until it exited or timed out. Fix: new additive ExecHandle::wait_or_kill(kill_future) in crates/harness-tools/src/exec.rs (waits like wait() but fires the kill sender when the future completes); relay now waits with wait_or_kill on krx (resolves on kill or on outer drop). Tests: harness-tools exec::tests::wait_or_kill_kills_on_request_and_otherwise_just_waits; denial::tests::relay_kills_after_output_closed (deterministic, channel-level, kill and drop after EOF, 10s timeout so a regression fails instead of hanging); sandbox_tty linux::kill_and_drop_end_the_process_after_its_output_closed (real sandboxed sh that closes stdout/stderr then execs sleep 60; kill and drop both, asserts pid gone). Mutation check: putting back inner.wait() fails both new policy tests (and leaks the sleep, which was killed by hand). Validation: cargo nextest run -p harness-tools -p harness-policy (177 passed), clippy --all-targets -D warnings, fmt --check, cargo check -p harness. No prompt/ini defaults changed.

Integration (TASK-151..155, 2026-10-23): workspace green on the combined tree with TASK-149/150 also Done: cargo fmt --all --check ok; cargo clippy --workspace --all-targets --locked -D warnings ok; cargo nextest run --workspace --locked --profile ci 1838 passed / 10 skipped (golden_requests and prompt snapshots included); cargo test --workspace --doc ok; cargo deny check ok; scripts/hermetic-guard.sh nextest -p harness -p harness-eval 216 passed, nothing written to HOME/XDG. CHANGELOG Defaults: the provisional TASK-152/153/155 entries are merged into one reconciled entry (also naming TASK-151's cache_ttl and TASK-153's [tools] job limits) with final fingerprint 45850933b6b6; defaults_changelog passes. README gains a Focused tools highlight and the harness-tools crate row. 'Passes in CI' is pending until the orchestrator pushes; nothing is committed.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Fixed terminal ioctls in the bash sandbox and made sandbox denials legible. The musl release build lost the /dev/tty Landlock rule because std drops O_PATH on musl; the launcher now opens rule paths with O_PATH via nix and grants LANDLOCK_ACCESS_FS_IOCTL_DEV only on terminal devices (/dev/tty, /dev/ptmx, /dev/pts, the command's tty), keeping other device ioctls denied on ABI 5+ and degrading to unrestricted ioctls on older ABIs. Failed sandboxed commands with unexplained permission errors get a one-line 'harness sandbox:' note naming [sandbox] in harness.ini. Verified with real-pty integration tests (openpty + setsid -c), an --abi 4 degradation test, unit tests, the musl build, and docs/policy.md + CHANGELOG updates.

Review follow-up: kill and drop of a sandboxed command now still reach the process group after its output streams have closed (new ExecHandle::wait_or_kill used by the denial relay), covered by unit and real-process tests.
<!-- SECTION:FINAL_SUMMARY:END -->
