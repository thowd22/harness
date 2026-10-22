---
id: TASK-137
title: Run CI on GitHub Actions and keep it green
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-22 19:52'
updated_date: '2026-10-22 20:36'
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
<!-- SECTION:NOTES:END -->
