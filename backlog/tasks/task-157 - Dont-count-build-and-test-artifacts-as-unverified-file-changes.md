---
id: TASK-157
title: Don't count build and test artifacts as unverified file changes
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 19:43'
updated_date: '2026-10-23 21:08'
labels:
  - tui
  - hygiene
dependencies: []
priority: medium
ordinal: 164000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Seen in a live Opus 5.5 TUI turn (2026-10-23): the model only ran ./run_tests.sh (a Python suite) and answered, yet the turn ended with '↻ unverified: 4 file changes and no verify command ran' (crates/harness-tui/src/chat/transcript.rs verification_notice, fed by turn_changes from file_changed events / the change ledger). The 4 changes were almost certainly __pycache__ .pyc files written by the test run, which is misleading: the user and model did not change any file.

Find where bash-run file changes become file_changed events (harness-hygiene change ledger / snapshot detection) and classify generated artifacts: files ignored by the project's .gitignore and well-known build/test output (__pycache__, *.pyc, .pytest_cache, target/, node_modules/, .mypy_cache, coverage files, .DS_Store) are recorded as generated, not user changes. They stay visible in the diff/ledger as generated but do not count toward 'unverified N file changes', verify_when = on_changes, or checkpoint noise. Source files changed by bash (sed -i, formatters) still count.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 A turn whose bash run only writes ignored or well-known artifact files (e.g. __pycache__/*.pyc) ends without an 'unverified' notice and does not trigger on-changes verification
- [x] #2 Files changed by bash that are tracked or not ignored still count and still produce the notice
- [x] #3 The change ledger / diff view shows generated files separately (or labelled generated) rather than dropping them silently
- [x] #4 Tests cover gitignored paths, built-in artifact patterns without a .gitignore, and a real bash run of a python test suite; docs (hygiene/verification) describe the rule
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
1. Reproduce: real binary (--fake-script, headless --json) runs a python unittest suite via bash in a temp git repo; show the file_changed events and the transcript's unverified notice.
2. harness-hygiene: new artifacts module classifying generated paths (project .gitignore incl. parent dirs/info/exclude, plus a short built-in list: __pycache__, *.pyc/*.pyo, .pytest_cache, .mypy_cache, .ruff_cache, .tox, .nox, target, node_modules, .coverage(.*), coverage.xml, htmlcov, .nyc_output, .DS_Store). Snapshot keeps skipping gitignored paths (git-like) and also honours parent .gitignore when the workspace is a subdirectory of the repo; bash changes matching the built-in list are recorded with generated=true (LedgerEntry/NetChange) instead of dropped.
3. harness-events: FileChanged gains generated: bool (serde default, skipped when false) so the log/diff/transcript can label them.
4. Verification: bash wrapper reports per-call whether it made non-generated changes (new ToolEvent::WorktreeChanges consumed by dispatcher into CallOutcome.changed); agent_loop st.changed uses it so verify_when=on_changes is not triggered by artifacts only.
5. TUI: transcript turn_changes counts only non-generated changes; diff view labels generated files.
6. Checkpoints: shadow repo info/exclude gets the built-in artifact list.
7. Tests: unit (classifier, snapshot/ledger with/without .gitignore), agent loop on_changes, transcript notice, diff label, e2e real binary python suite. Docs: hygiene/verification docs + CHANGELOG.

Review follow-up (findings 1-4):

8. Snapshot: apply parent .gitignore files only up to the nearest repository root (first ancestor incl. the workspace with .git). Workspace at its repo root or outside a repo: parents(false) as before; below the root: parents(true)+require_git(true) so the ignore crate stops at that .git. Tests: dotfiles-style outer repo with '*' around a nested repo; non-repo .gitignore above the repo root.

9. Ledger: a bash change is generated only if it matches BUILTIN_ARTIFACTS and the user's git does not track it (one git ls-files over the candidates per command, literal pathspecs, GIT_* scrubbed). Checkpoints: force-add user-tracked paths that match the built-in list so they are still checkpointed and restored. Tests: tracked infra/target file via sed -i counts (ledger + on_changes verification + checkpoint/undo).

Integration follow-up: 10. Snapshot: below the repo root, apply parent .gitignore files only when the enclosing repository does not ignore the workspace directory itself (ignored_by_parents: parent .gitignore files + info/exclude, deepest first); otherwise only the workspace's own rules, as before TASK-157. In a repository, add the tracked files an ignore rule hides (git ls-files --cached --ignored --exclude-standard), since git never ignores tracked files. Tests: snapshot unit tests (dotfiles home '*' with force-added config dir and plain project; tracked force-added *.log below and at the repo root) and a real-binary e2e test.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Repro (real binary, headless --json --fake-script, temp git repo, python unittest suite via ./run_tests.sh, no .gitignore): 3 file_changed events, all calc|tests/__pycache__/*.cpython-314.pyc; transcript notices BEFORE = ["unverified: 3 file changes and no verify command ran"], AFTER = [] (same events now carry generated:true).
Decisions: gitignored files stay out of snapshots entirely (git-status semantics; walking target/ or node_modules to label them would be expensive), now also honouring parent .gitignore files when the workspace is below the repo root. Built-in list (harness_hygiene::artifacts::BUILTIN_ARTIFACTS) classifies bash changes as generated; write/edit changes are never generated. Generated bash entries drop their diff. on_changes: LedgerBash emits ToolEvent::WorktreeChanges{files,generated}; Dispatcher captures it per call into CallOutcome::changed; agent_loop uses it (fallback: mutating && !error). Reducer files_changed skips generated. Checkpoint shadow repo writes info/exclude with the built-in list. /diff lists generated files last with '· generated' label (no new interactive elements). No prompt/tool-description/ini default changes, so no Defaults entry.
Validation: cargo nextest -p harness-events -p harness-hygiene -p harness-tools -p harness-agent -p harness-tui -p harness -p harness-remote -p harness-workflow -p harness-eval: 1208 passed; the 2 failures are in harness-tui layout_matrix (another engineer's untracked in-progress module), as is the only clippy error. clippy -D warnings clean for my crates; rustfmt clean on touched files.

Review follow-up: all 4 findings verified as real and fixed. (1/3) Snapshot read .gitignore files up to / (require_git(false) never stops at a .git). Now: nearest repository root = first of the workspace and its ancestors with .git; at that root or outside a repo, parents(false)/require_git(false) as before TASK-157; below it, parents(true)+require_git(true), so the ignore crate stops at that .git (ignore-0.4.33 dir.rs saw_git). New test a_gitignore_above_the_repository_root_does_not_apply (dotfiles home with '*' around a nested repo, workspace at and below the nested root; non-repo outer .gitignore above the repo) failed on the old logic (left: [], right: ['.gitignore','main.py']) and passes now. (2/4) Generated now requires a BUILTIN_ARTIFACTS match AND not tracked by the user's git: artifacts::tracked_among runs one 'git --literal-pathspecs ls-files -z -- <candidates>' (chunked args, GIT_* scrubbed) per bash command, only when there are candidates; ls-files does not support --pathspec-from-file. Checkpoints: stage_all force-adds (add -f via --pathspec-from-file stdin) user-tracked paths matching the list (Artifacts::tracked_in) so they are checkpointed and restored. Tests: artifacts tracked_files_matching_the_list_are_found, ledger tracked_files_matching_the_artifact_list_still_count (workspace below repo root; tracked infra/target, src/target, deleted coverage.xml count; untracked target/ and __pycache__ stay generated), checkpoints tracked_files_matching_the_artifact_list_are_checkpointed, e2e generated_changes a_tracked_file_under_a_target_directory_still_counts_and_is_verified (real binary, make test on_changes verify_started) and a_gitignore_above_the_repository_does_not_hide_bash_changes (dotfiles home; 2 file_changed, 'unverified: 2 file changes' notice). Used printf instead of sed -i in the e2e for macOS portability. Docs (hygiene.md) and the CHANGELOG Fixed entry updated; no prompt/tool/ini default changes. Validation: nextest -p harness-hygiene -p harness-agent -p harness -p harness-tools -p harness-events 510/510 passed; -p harness-tui -p harness-remote -p harness-workflow -p harness-eval 705/706, the failure is layout_matrix (another engineer's in-progress TASK-160 module). commands::workflow_agent_steps_get_job_notices_and_their_jobs_end_with_the_run failed once under parallel compile load (j2 not yet 'stopped') and passed 3/3 alone and in the full rerun; it does not touch artifact paths. clippy -D warnings clean for harness-hygiene and harness tests; rustfmt clean on touched files.

Integration fix for the remaining finding (dotfiles HOME with '*' hid tracked files and plain projects below it when the workspace is below the repo root): snapshot::ignored_by_parents decides whether the enclosing repository ignores the workspace directory (then parents(false)/require_git(false) as before TASK-157), and artifacts::tracked_but_ignored (one 'git ls-files -z --cached --ignored --exclude-standard' per snapshot, only inside a repository, GIT_* scrubbed) adds tracked files hidden by any ignore rule, also at the repo root. New tests: snapshot a_workspace_ignored_by_its_enclosing_repository_sees_its_files, tracked_files_are_listed_even_when_an_ignore_rule_matches_them (both failed before), e2e generated_changes bash_changes_in_a_directory_a_dotfiles_home_ignores_still_count (failed with the fix disabled: left [] vs main.py, tracked.txt). Docs hygiene.md and CHANGELOG Fixed entry updated.

Integration validation (2026-10-23, tree including other engineers' in-progress TASK-160/161 work): cargo fmt --all --check, cargo clippy --workspace --all-targets --locked -D warnings, cargo nextest run --workspace --locked --profile ci x3 (1948/1948 passed each run, no flakes), cargo test --workspace --doc, cargo deny check, scripts/hermetic-guard.sh nextest run -p harness -p harness-eval (230 passed, nothing written to HOME/XDG), no .snap.new files, defaults_changelog passes (no defaults changed). CI part of DoD#1 pending until the orchestrator pushes.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Bash-made changes to well-known build/test artifacts (__pycache__, *.pyc, .pytest_cache, .mypy_cache, target/, node_modules/, coverage files, .DS_Store, ...) are recorded as generated (harness_hygiene::artifacts, LedgerEntry/NetChange.generated, file_changed.generated): visible in node detail and /diff (labelled generated), but not counted toward the unverified notice, verify_when=on_changes, the files-changed count or checkpoints. Parent .gitignore files apply below the repo root only up to that root and only when that repository does not ignore the workspace itself (dotfiles home with '*'); tracked files always count, also when an ignore rule matches them or they match the built-in list. Verified with unit tests (artifacts, snapshot, ledger, checkpoints, schema, transcript, diff view), agent-loop tests (verify_on_changes.rs) and real-binary e2e tests (generated_changes.rs: python suite, tracked target/ file, nested repo in a dotfiles home, plain project and force-added file in a dotfiles home); full workspace gates green locally, CI pending push.
<!-- SECTION:FINAL_SUMMARY:END -->
