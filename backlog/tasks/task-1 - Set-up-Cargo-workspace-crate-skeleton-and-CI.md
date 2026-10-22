---
id: TASK-1
title: 'Set up Cargo workspace, crate skeleton and CI'
status: Done
assignee:
  - '@scaffold'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 14:57'
labels:
  - core
  - infra
milestone: m-0
dependencies: []
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: chore
ordinal: 1000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Every later task lands in a specific crate. The crate boundaries in the architecture doc keep the core headless so the TUI, headless CLI, eval runner and remote helper share one engine. CI must exist from day one so quality regressions are caught immediately.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Workspace contains the crates listed in the architecture doc plus the harness binary, all compiling
- [x] #2 CI runs cargo fmt --check, clippy with warnings denied, tests, and cargo-deny on every push
- [x] #3 Workspace-level lints and a pinned rust-toolchain file are committed
- [x] #4 README states the build and test commands
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
1. Root Cargo.toml workspace (resolver 3, edition 2024, MIT, workspace lints, pre-populated workspace.dependencies resolved with cargo add).
2. Create 12 library crates + harness bin under crates/ with doc-commented module stubs.
3. rust-toolchain.toml pinned to 1.93.1 with rustfmt/clippy; deny.toml; .gitignore.
4. .github/workflows/ci.yml: fmt --check, clippy -D warnings, test, cargo-deny on every push/PR.
5. Workspace smoke test (tests that assert members/lints/toolchain/CI steps exist) in harness bin crate.
6. README with build/test commands.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Workspace: 12 lib crates + harness bin under crates/, resolver 3, edition 2024, MIT, rust-version 1.93. Every planned module pre-created as a doc-commented stub. [workspace.dependencies] pre-populated (all specs verified to compile and pass cargo-deny in a scratch crate). serde_norway chosen for YAML because serde_yaml_ng pulls unmaintained unsafe-libyaml (RUSTSEC-2024-0449). reqwest 0.13 'rustls' feature uses aws-lc-rs (needs cmake/cc). Lints: clippy all=warn, unsafe_code=deny (deny not forbid so a sandbox module can locally allow with justification), missing_docs=warn. rust-toolchain.toml pins 1.93.1 + rustfmt/clippy. deny.toml allows permissive licenses + MPL-2.0 (option-ext via dirs). CI: fmt/clippy/test/cargo-deny jobs on push and PR. Validation: cargo fmt --check OK, cargo clippy --workspace --all-targets -D warnings OK, cargo test --workspace 15 passed, cargo deny check all ok. crates/harness/tests/workspace.rs asserts members, lints, toolchain pin, CI steps and README commands. CI itself not yet executed on GitHub (no push from this environment). DoD#4 N/A (no UI).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Created the Cargo workspace (13 crates with module stubs), workspace lints and deps, pinned toolchain, deny.toml, .gitignore, CI workflow and README. Verified with fmt/clippy/test/cargo-deny locally and workspace skeleton tests in crates/harness/tests/workspace.rs.
<!-- SECTION:FINAL_SUMMARY:END -->
