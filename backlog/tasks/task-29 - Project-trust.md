---
id: TASK-29
title: Project trust
status: Done
assignee:
  - '@config-ini'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:09'
labels:
  - security
  - config
  - problem-8
milestone: m-2
dependencies:
  - TASK-2
  - TASK-7
priority: high
type: feature
ordinal: 29000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Starting the harness in a malicious repository must not execute or load anything from it (a real CVE class in existing harnesses).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Project .harness directory is ignored until the user trusts the project
- [x] #2 Trust records a hash of the project config and changes re-prompt with a diff
- [x] #3 Untrusted projects still work with global config only
- [x] #4 Trust store is a protected path
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
1. trust::snapshot_project fingerprints .harness/ (sha256 per file + text copy, symlinks not traversed).
2. TrustStore under <data_dir>/trust/projects/<sha(root)>.json, 0600, atomic.
3. status() -> NoProjectConfig | Untrusted | Trusted | Changed{diff}; trust(root, reviewed) refuses if contents changed since review.
4. trusted_config_dir() is the only gate for loading project config.
5. docs/trust.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
crates/harness-config/src/trust.rs. Diff via similar unified diff (trusted/<path> vs current/<path>). Top-level .harness symlink rejected (NotPlainDirectory). Tests: trust::tests (ignored until trusted, change re-prompts with diff and blocks loading, ChangedSinceReview, revoke, symlink handling, record mode 0600, corrupt record) and tests/trust_global_only.rs (untrusted project with global config; trust store inside protected paths). Cross-crate: harness_config::discovery (YAML owner) must obtain the project dir only via TrustStore::trusted_config_dir and treat errors as untrusted; harness-tui must render the prompt (Untrusted contents / Changed diff) and call trust(root, &snapshot). DoD#4 N/A: no TUI elements in this task. Verified: cargo test -p harness-config (42 unit + 5 integration + 1 doctest pass), cargo clippy -p harness-config --all-targets -- -D warnings clean, RUSTDOCFLAGS=-D warnings cargo doc clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Hash+snapshot trust store with diff re-prompt and trusted_config_dir gate; verified by trust unit tests and integration tests.
<!-- SECTION:FINAL_SUMMARY:END -->
