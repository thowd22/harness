---
id: TASK-142
title: 'Apply open Dependabot updates (actions, zstd, base64)'
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-23 02:40'
updated_date: '2026-10-23 03:25'
labels:
  - infra
  - deps
milestone: m-11
dependencies:
  - TASK-137
priority: medium
type: chore
ordinal: 142000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Dependabot opened PR #1 (GitHub Actions group: checkout 4->7, dtolnay/rust-toolchain 1.93.1->1.120.0, setup-zig 1->2, upload-artifact 4->7, download-artifact 4->8), #2 (zstd 0.13.3->0.14.3) and #3 (base64 0.22.1->0.23.1). Apply the safe parts in-tree with tests, without moving the CI compiler off rust-toolchain.toml and without breaking the remote helper wire format for helpers already deployed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Workflows use actions/checkout@v7, mlugg/setup-zig@v2, actions/upload-artifact@v7 and actions/download-artifact@v8, with release artifact upload/download semantics verified
- [x] #2 dtolnay/rust-toolchain stays at the rust-toolchain.toml channel; a test fails if any workflow tag differs, Dependabot ignores the action, and docs/ci.md says how the toolchain is upgraded
- [x] #3 zstd 0.14.3 and base64 0.23.1 are in Cargo.toml and Cargo.lock with any code changes they need
- [x] #4 Golden-frame tests prove protocol 2 zstd frames and protocol 1 base64 lines from helpers built with zstd 0.13.3 / base64 0.22.1 still decode and that the encoder output keeps their shape
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
1. Read PRs #1-#3 and the release notes / action.yml of each new action version.
2. Capture golden protocol frames (zstd frame, base64 line) with zstd 0.13.3 / base64 0.22.1 before bumping; add crates/harness-remote/tests/wire_compat.rs.
3. Bump zstd and base64 in Cargo.toml/Cargo.lock; build and test harness-remote and harness-providers.
4. Bump checkout/setup-zig/upload-artifact/download-artifact in all workflows; keep dtolnay/rust-toolchain at rust-toolchain.toml.
5. Dependabot ignore rule for dtolnay/rust-toolchain, docs/ci.md toolchain-upgrade procedure, structural tests in crates/harness/tests/ci_workflow.rs.
6. actionlint, cargo deny, clippy, fmt, tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Decisions and verification:
- dtolnay/rust-toolchain NOT bumped (PR #1 wanted 1.120.0): its tag is the Rust version, so it would diverge from rust-toolchain.toml (1.93.1). Added a Dependabot ignore for it and docs/ci.md 'Upgrading the Rust toolchain'. New test ci_workflow::workflows_install_the_pinned_toolchain fails if any workflow tag differs from rust-toolchain.toml (mutation-checked: changing one tag to 1.120.0 fails it); dependabot_leaves_the_toolchain_pin_alone checks the ignore rule.
- actions/checkout@v7: no input we use changed; the fork-PR block only affects pull_request_target/workflow_run, which we do not use; persist-credentials still defaults to true, so cut-release.yml's git push keeps working.
- mlugg/setup-zig@v2: version default changed from 'latest' to '' = minimum_zig_version from build.zig.zon, falling back to latest; there is no build.zig.zon, so behaviour is unchanged. Added cache-key: matrix.target in release-harness.yml build job (v2 feature) since both musl targets share ubuntu-latest.
- upload-artifact@v7 / download-artifact@v8: we upload directories (dist/, remote-bin/) with archive defaulting to true, so artifacts are still zips with the same names; download-artifact v8 still unzips zip artifacts, pattern + merge-multiple unchanged; the new digest-mismatch=error default only fails on corrupted downloads. No hidden files in dist/. Semantics used by release-harness.yml (dist-* -> dist, sha256sum -c, 6 tarballs) and release.yml hold.
- zstd 0.14.3 / base64 0.23.1: no code changes needed (same libzstd 1.5.7; base64 STANDARD engine API unchanged). Lockfile diff is identical to PRs #2 and #3 (base64 0.22.1 stays for other crates). Encoder output for the golden zstd frame is byte-identical before and after the bump (checked with a throwaway capture test). wire_compat.rs: 4 tests (old zstd frame and base64 line decode; current encoder keeps header shape, standard zstd magic, no dictionary, bounded one-shot decompress as older helpers do; base64 line byte-identical).
- Checks: nextest -p harness-remote -p harness-providers 381 passed; ci_workflow + workspace 14 passed; clippy -D warnings clean on harness-remote, harness-providers, harness ci_workflow test; actionlint 1.7.12 clean; cargo deny check ok; YAML parses.
- Full workspace nextest: 1431 passed, 7 failed, all outside this task (harness-eval golden_requests and defaults_changelog = identity lane prompt work; harness-tui input::tests::ranks_1000_commands_within_a_frame_per_keystroke = fuzzy lane).
- Pending: DoD#1 'pass in CI' after the orchestrator pushes.

Integration (2026-10-23): with every lane merged, the full workspace is green locally (nextest ci profile 1452 passed; clippy, fmt, doc tests, cargo deny, install-test, shellcheck clean). The earlier failures in other lanes (golden_requests, defaults_changelog, the fuzzy frame-budget test, fmt diffs) no longer reproduce. DoD#1 still waits for CI after the push.
<!-- SECTION:NOTES:END -->
