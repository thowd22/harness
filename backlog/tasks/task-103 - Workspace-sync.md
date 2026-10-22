---
id: TASK-103
title: Workspace sync
status: Done
assignee:
  - '@remote'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:21'
labels:
  - remote
  - hygiene
milestone: m-10
dependencies:
  - TASK-102
  - TASK-72
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: high
type: feature
ordinal: 103000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Remote nodes need the project files, and their changes must come back safely.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Syncs worktree honoring gitignore to a per-run remote dir via rsync or git bundle
- [x] #2 Changes return as a patch that passes policy and hygiene checks before being applied locally
- [x] #3 Conflicts with local edits are surfaced and never overwritten
- [x] #4 Remote run dirs are cleaned up by policy
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
1. Manifest honouring gitignore. 2. Push by rsync (files-from) or git bundle snapshot to <workdir>/<project-key>/<run-id>, verify remote copy. 3. Helper sync.changes -> ChangeSet; patch rendering. 4. All-or-nothing apply through ChangeGates (policy, hygiene) with conflict detection vs. the pushed baseline. 5. CleanupPolicy + remove/prune confined to the project dir. 6. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
sync.rs:
- manifest(root) uses the ignore crate. It honours .gitignore even without a repo, includes hidden and untracked files, and skips .git/. Entries carry sha256, mode and symlink target.
- Push: rsync_args/rsync_files_from/rsync_push (--files-from list over the same ssh options), or create_bundle (a snapshot commit in a temporary index plus a temporary ref; the user's index, HEAD and branches stay untouched) with unbundle_script.
- RemoteRun::push(client, local_root, workdir, run_id, &Pusher) copies to <workdir>/<project-key>/<run-id>, then checks the remote manifest against what was pushed (SyncError::Incomplete if they differ).
- RemoteRun::pull uses helper sync.changes and returns a ChangeSet of FileDelta{path, kind, before, after, mode, content, symlink}. ChangeSet::to_patch renders a unified diff.
- apply_changes / RemoteRun::apply is all-or-nothing. It runs the ChangeGates (PolicyGate runs each path as Action::Write through PolicyEngine; HygieneGate rejects temp files and paths outside the scope), rejects escaping paths, and detects conflicts against the local baseline. Ask verdicts need ApplyOptions{approved: true}.
- Cleanup: CleanupPolicy {Always, OnSuccess (default), Never}, RemoteRun::cleanup and prune. The helper's sync.remove and sync.prune refuse paths outside the project dir.

Tests:
- sync::tests: manifest, rsync push, rsync args with ssh, git bundle with untouched user repo, change detection, apply, conflicts, identical-change no-conflict, hygiene, policy gate deny/ask/approve, escaping paths, run dirs, cleanup policy, remove/prune.
- tests/sync.rs end to end over the helper: rsync and git roundtrips with edits made through RemoteWorkspace, a conflict surfaced without overwriting, failed runs kept then pruned, removal outside the base refused, the push audited.

Integration still to do in other crates (outside these ACs):
- harness-hygiene should implement harness_remote::sync::ChangeGate for its scope and temp-file checks.
- The TASK-72 change ledger should record applied remote changes.
- harness-config could add `[remote.<name>] cleanup = always|on-success|never` (RemoteHost::cleanup() returns the default for now).
- The runs_on scheduler (TASK-104) calls RemoteRun::push, pull, apply and cleanup around a remote node.
cargo test -p harness-remote: 91 tests pass (64 unit, 27 integration in tests/{conformance,failures,install,sync,tools_remote}.rs), 1 #[ignore] real-sshd test; cargo test -p harness --test remote_agent: 2 pass; cargo clippy -p harness-remote --all-targets -- -D warnings clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Workspace sync in harness_remote::sync: gitignore-aware manifest, rsync or git-bundle push to a per-run remote dir with verification, change pull as a ChangeSet/patch, all-or-nothing apply gated by policy (PolicyGate) and hygiene (HygieneGate) with conflict detection that never overwrites local edits, and policy-driven cleanup/prune confined to the project dir. Verified by sync unit tests and tests/sync.rs end-to-end over the helper. DoD#4 N/A (no TUI).
<!-- SECTION:FINAL_SUMMARY:END -->
