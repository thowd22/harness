---
id: TASK-137
title: Run CI on GitHub Actions and keep it green
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-22 19:52'
updated_date: '2026-10-23 03:25'
labels:
  - infra
  - testing
  - problem-9
milestone: m-11
dependencies:
  - TASK-1
priority: high
type: chore
ordinal: 137000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The repository now has a GitHub remote (thowd22/harness) but CI has never run: nothing has been pushed. Tests that pass locally may fail on hosted runners (no Landlock, different kernel, no TTY, missing tools), and the team needs a green signal on every push and pull request.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 ci.yml runs fmt, clippy with warnings denied, the full test suite and cargo-deny on push and pull request, on ubuntu-latest and macos-latest
- [x] #2 Tests that depend on host features (Landlock, bwrap, sshd, a TTY) detect their absence and are skipped with a logged reason, never silently passing a weakened assertion
- [ ] #3 Caching keeps a no-change CI run under 10 minutes
- [ ] #4 The first CI run on GitHub is green, with a link to the passing run recorded in the task notes
- [x] #5 README shows a CI status badge
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [x] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [x] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [x] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->

## Implementation Plan

<!-- SECTION:PLAN:BEGIN -->
1. Audit tests for host features (Landlock, bwrap/unshare, sshd, /proc, TTY) and macOS cfg issues.
2. cfg-clean the workspace for aarch64-apple-darwin (clippy -D warnings via zig cc wrapper).
3. Replace /proc-only liveness checks with a portable helper (ps off Linux) so macOS never passes vacuously; log SKIP reasons in sandbox tests' degraded branches.
4. Rewrite ci.yml: fmt, clippy + test matrix ubuntu/macos, cargo-deny, rust-cache, timeouts, host-feature log step.
5. README badge, docs/ci.md, structural test of ci.yml (crates/harness/tests/ci_workflow.rs).
6. Validate locally: actionlint, cargo deny, CI-like env test run (env -i, empty HOME, no TTY, symlinked TMPDIR).
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented locally (nothing pushed yet).
- ci.yml: fmt; clippy and test on an ubuntu-latest/macos-latest matrix (fail-fast off); cargo-deny; RUSTFLAGS=-D warnings; Swatinem/rust-cache per OS+job (saved only from pushes); timeouts; PR runs cancel when superseded; test job logs host features (bwrap/unshare/sshd/tty, harness-sandbox --probe) and reruns sandbox tests with --nocapture so SKIP lines show. Also workflow_dispatch and workflow_call (lets release.yml reuse it). actionlint 1.7.7 clean; cargo deny check clean; cargo metadata --locked ok.
- macOS: workspace clippy --all-targets --target aarch64-apple-darwin -D warnings is clean (zig cc wrapper for aws-lc-sys). Fixed dead-code warning on non-Linux in harness-policy sandbox/launcher.rs (Applied::Full/Partial).
- Host-dependent tests: /proc-only liveness checks (harness-tools bash.rs, harness-remote shell.rs and tests/failures.rs) passed vacuously without /proc; now use harness_tools::conformance::local_process_gone (ps off Linux, both paths unit-tested); exec_suite shell probe falls back to ps. Sandbox tests log 'SKIP enforcement check: <reason>' when they take the degraded-path assertions. Remote helper socket tests use /tmp socket dirs (macOS 104-byte sun_path with ~50-byte TMPDIR would fail). harness-workflow remote_steps accepts the resolved workdir (macOS /private/var symlink).
- Validated with a CI-like env: env -i, empty HOME, no TTY (setsid, stdin /dev/null), long symlinked TMPDIR. Remaining failures were not env-related: harness-eval defaults_changelog (defaults changed by the concurrent run, needs a CHANGELOG entry) and three harness-tui copy tests (in-progress copy/paste work); both fail in the normal env too.
- New tests: crates/harness/tests/ci_workflow.rs (workflow structure, badge, docs), conformance::tests (liveness helper).
- Docs: docs/ci.md, README badge and Development paragraph.
- AC#3 (no-change run < 10 min) and AC#4 (first green run link) await the push: after the orchestrator pushes, rerun CI with no changes, record durations and the run URL here, then check them.

Review follow-up (2026-10-22): no blocking findings. Re-verified: ci_workflow 7/7 pass, actionlint 1.7.7 clean on ci.yml, README badge and docs/ci.md present. DoD#2 checked (docs/ci.md, README; no new config keys or public APIs beyond rustdoc'd conformance::local_process_gone). DoD#3 checked (no check needed more than 3 fix attempts). DoD#4 N/A: no TUI-visible change. Still pending the push: AC#3 (warm-cache no-change run < 10 min), AC#4 (first green run link) and DoD#1 (pass in CI). Known blockers for a first green run, owned by the concurrent run, not this task: cargo fmt --check diff in their in-progress files, harness-eval defaults_changelog (needs CHANGELOG entry), three harness-tui copy tests.

CI-hazard audit (2026-10-23), each finding verified before it was fixed, and each fix has a test that fails without it:
1. [fixed] harness-remote shell.rs kill_group_script: dash and busybox ash reject `kill --`, and dash also rejects `kill -s TERM -PGID` (measured locally: 'Illegal option -7'), so the audit's suggested -s form was wrong for dash. The script now uses `kill -TERM|-0|-KILL -PGID`, verified under dash, busybox sh, bash and zsh. New test kill_script_stops_a_foreign_group_under_sh runs the script through sh against a group LocalExec does not own (red on the old script).
2. [fixed] ssh.rs control dir: candidates $XDG_RUNTIME_DIR/harness, $TMPDIR/harness-$USER, /tmp/harness-$USER; the first whose dir+/cmz-<40>+.<16> fits SUN_PATH_MAX (104 on macOS/BSD, 108 on Linux) wins. create_private_dir refuses a symlink, and set_permissions fails on a dir someone else owns. Tests: control_dir_fits_sun_path_with_a_macos_tmpdir, private_dir_refuses_a_symlink.
3. [fixed] harness-eval run_shell and harness-config run_secret_command: process_group(0) plus a guarded killpg(SIGKILL) on timeout (the pgid<=1 guard is kept). Tests shell_timeout_kills_background_children (red before) and cmd_timeout_kills_background_children. harness-config now depends on nix (workspace dep).
4. [fixed] harness-tools LocalExec: reproduced the SIGTTIN stop (state T) with a pty session leader. The child now calls setsid() in pre_exec (#[allow(unsafe_code)] with a SAFETY note). process_group(0) had to be dropped because setsid fails with EPERM on a group leader. terminate_group sends SIGCONT after SIGTERM. Tests: exec_child_leads_its_own_session (Linux, /proc), kill_continues_a_stopped_group (both red before), tty_open_fails_fast_instead_of_stopping (passes either way without a ctty, so it documents the behaviour).
5. [fixed, wider than reported] With GIT_DIR/GIT_INDEX_FILE/GIT_WORK_TREE exported, 3 tests still failed after the helpers were scrubbed, because production code also honoured them: hygiene tracked_files, memory remote_url, and remote sync create_bundle plus unbundle_script (which wrote objects into the outer repo). These now remove the repo-location variables, following drive/git.rs. With them exported, hygiene+memory+remote: 259/259 pass and the outer repo is untouched. git_bundle_carries_untracked_and_respects_ignore now runs the unbundle with a stray GIT_DIR (red before).
6. [fixed] The fake ssh (ssh.rs) and fake gh (tests/release.rs) are now created with `install -m 755` from a child process, like write_stub.
7. [fixed] build-remote-helpers.sh sources release-common.sh and uses write_sha256. verify-remote-helpers.sh uses sha256_of, says it is Linux-only, and dies early when readelf or timeout is missing. Also quoted $runner (SC2086) and added a shellcheck shell directive to release-common.sh. Tests: remote_helper_scripts_use_portable_checksums, verify_remote_helpers_checks_the_sidecar.
Integration checks: cargo fmt --all --check clean; cargo clippy --workspace --all-targets --locked -D warnings clean; cargo nextest run --workspace --locked --profile ci 1452 passed / 8 skipped; cargo test --workspace --locked --doc ok; cargo deny check ok; bash tests/install-test.sh passed; shellcheck -x on scripts/*.sh, tests/*.sh, install.sh and the fixtures clean; actionlint reports only the valid macos-15-intel label.
<!-- SECTION:NOTES:END -->
