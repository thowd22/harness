---
id: TASK-138
title: Automated releases gated on tests
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-22 19:52'
updated_date: '2026-10-22 20:23'
labels:
  - infra
  - release
milestone: m-11
dependencies:
  - TASK-137
  - TASK-101
priority: high
type: feature
ordinal: 138000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users need downloadable binaries, and releases should only ever be cut from a commit whose tests pass. The remote helper already has musl build scripts (TASK-101) that the release should reuse.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Pushing a v* tag runs the full CI first; the release is created only if every CI job passes
- [ ] #2 A workflow_dispatch Cut release workflow takes a version, checks CI is green on that commit, bumps the workspace version and CHANGELOG, tags and triggers the release
- [ ] #3 Release artifacts: harness binaries for linux x86_64 and aarch64 (musl, static) and macOS arm64 and x86_64, plus the remote helper builds, each with a sha256 file
- [x] #4 Release notes come from the CHANGELOG section for that version
- [x] #5 harness --version reports the released version and git commit
- [ ] #6 A first release (v0.1.0 or the next version) is cut successfully on GitHub and its artifacts download and run, with the link recorded in the task notes
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
1. Make ci.yml reusable (additive on: workflow_call) so the release workflow runs the full CI on the exact release commit first.
2. .github/workflows/release-harness.yml: on v* tag push or workflow_dispatch(tag); jobs ci (uses ci.yml) -> build matrix (linux x86_64/aarch64 musl via cargo-zigbuild on ubuntu, macOS arm64 on macos-latest, x86_64 on macos-15-intel) + remote helpers via scripts/build-remote-helpers.sh -> release job (needs all) creating the GitHub release with notes from CHANGELOG.
3. .github/workflows/cut-release.yml: workflow_dispatch(version); gate on a successful CI run for the commit (scripts/release-ci-gate.sh), bump workspace version + Cargo.lock + CHANGELOG (scripts/release-bump.sh), commit, tag, push, dispatch release-harness.yml for the tag.
4. scripts/release-notes.sh (extract CHANGELOG section), scripts/release-package.sh (tar.gz + sha256), portable POSIX sh (linux + macOS).
5. crates/harness/build.rs: embed git commit (HARNESS_GIT_COMMIT override, else git rev-parse, else unknown); --version prints 'harness X.Y.Z (commit)'.
6. CHANGELOG: add ## Unreleased section above ## Defaults.
7. Tests in crates/harness/tests/release.rs: run scripts against temp fixtures and a fake gh, parse workflows and assert the needs-graph gates the release on CI, --version output.
8. docs/releasing.md; validate with actionlint, cargo test/clippy -p harness.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented locally (not pushed; orchestrator pushes):
- .github/workflows/release-harness.yml: on v* tag push or workflow_dispatch on a tag. Job ci = uses ./.github/workflows/ci.yml (ci.yml gained on: workflow_call, kept by the TASK-137 rewrite); meta checks tag == v+workspace version and CHANGELOG notes; build matrix (x86_64/aarch64 musl via cargo-zigbuild on ubuntu, aarch64-apple-darwin on macos-latest, x86_64-apple-darwin on macos-15-intel) and helpers (scripts/build-remote-helpers.sh + verify-remote-helpers.sh REQUIRE_RUN=1) need [ci, meta]; publish needs all, checks every sha256 and creates the release with notes from CHANGELOG.
- .github/workflows/cut-release.yml: workflow_dispatch(version): release-ci-gate.sh on the branch head, release-bump.sh, cargo metadata --locked, commit + annotated tag + atomic push, then gh workflow run release-harness.yml --ref vX.Y.Z (GITHUB_TOKEN tag pushes do not trigger push workflows).
- scripts/release-{common,bump,notes,version,ci-gate,smoke,package}.sh (POSIX sh, Linux+macOS; shellcheck clean).
- crates/harness/build.rs embeds HARNESS_GIT_COMMIT (env override, else git rev-parse HEAD, else unknown); --version prints 'harness X.Y.Z (commit12)'.
- CHANGELOG.md: '## Unreleased' section (first-release entries) above '## Defaults'.
- docs/releasing.md (new), docs/guide/install.md and docs/cli.md updated.
Validation: cargo test -p harness --test release (11 tests: bump/notes/version/package/ci-gate with fake gh/smoke, workflow needs-graph and matrix parsed with serde_norway, --version) pass; cargo clippy -p harness --all-targets -D warnings clean; actionlint 1.7.12 with shellcheck clean on all workflows; local cargo zigbuild --release --locked of harness for x86_64 and aarch64 musl succeeded, both statically linked; x86_64 build with HARNESS_GIT_COMMIT set passes release-smoke.sh and packages with a verifying sha256; bump of the real tree to 0.2.0 passes cargo metadata --locked --offline.
Awaiting push: AC1, AC2, AC3 (workflow behaviour on GitHub, macOS builds) and AC6 (first release v0.1.0 + link) need a live run; DoD1 needs a CI run.
Not mine, seen while testing: harness-eval defaults_changelog fails (fingerprint 9615b4d09acd) from another agent's defaults change; harness workspace.rs ci_runs_fmt_clippy_test_and_deny_on_push fails against TASK-137's in-progress ci.yml (clippy line now has --locked).

Review fix (cut-release leading v): confirmed. The workflow built the tag name from the raw input, so version=v0.1.0 would have pushed tag vv0.1.0 and a release commit before release-harness.yml's meta job rejected it. Fixed: a new first step after checkout, 'Normalise and validate the version', strips one leading v, checks valid_version from scripts/release-common.sh, and writes the normalised VERSION to GITHUB_ENV. It runs before the tag-existence check, so every later step (tag check, bump, notes, tag, push, dispatch) uses the clean value. Input description and docs/releasing.md updated. Tests: cut_release_gates_on_ci_then_bumps_tags_and_triggers_the_release now asserts the step order validate < tag check < ci gate < bump < tag/push < dispatch and that no later step reads inputs.version. The new cut_release_normalises_a_leading_v_and_rejects_bad_versions runs the step's script with bash: v0.1.0, 0.1.0 and v1.2.3-rc.1 are accepted and normalised; vv0.1.0, empty, 0.1, 01.2.3, a trailing space and x1.2.3 are rejected and GITHUB_ENV is left untouched. Validation: cargo test -p harness --test release passes 12/12, and clippy -p harness --test release -D warnings is clean. Both ran in a scratch snapshot because the live tree does not compile: another agent's crates/harness/src/workflow.rs has secrets used after a move (E0382); the snapshot patches it with secrets.clone(). actionlint 1.7.12 with shellcheck 0.10.0 is clean on cut-release.yml and release-harness.yml. Still awaiting a live GitHub run: AC1, AC2, AC3, AC6 and DoD1.
<!-- SECTION:NOTES:END -->
