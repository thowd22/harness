---
id: TASK-41
title: 'Config discovery, layering and hot reload'
status: Done
assignee:
  - '@yaml-config'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:32'
labels:
  - config
milestone: m-4
dependencies:
  - TASK-42
documentation:
  - backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md
priority: high
type: feature
ordinal: 41000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Prompts, agents, workflows and skills come from built-in, global and trusted project scopes. Users need to know what is active and edit it without restarting.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Discovers files in built-in, global and trusted project scopes per the YAML spec
- [x] #2 Later scopes shadow earlier by name and shadowing is reported
- [x] #3 Files hot-reload; invalid edits keep the previous version and surface a located error
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
1. discovery: Scope{Builtin,Global,Project}, Kind{Prompt,Agent,Workflow,Skill}, Roots resolved from config dir + TrustStore::trusted_config_dir (Err = untrusted).
2. Scan kind dirs (yaml/yml, skills name.md or name/SKILL.md), never traverse symlinked dirs, skip symlinks in project scope.
3. Generic Registry<T: Artifact> layering by name, shadow report, scoped lookup for /global:name /project:name.
4. Catalog::reload keeps previous version of a file whose new contents fail and records a located error; Watcher wraps notify to trigger reload.
5. Tests with temp dirs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_config::discovery: Scope, Kind, BuiltinFile, Roots (resolve/resolve_with via TrustStore::trusted_config_dir, Err = untrusted; recheck_trust for reloads), SourceFile, Artifact trait, Registry<T> (load/reload/get/get_in/active/all/shadowed/shadowed_by/errors/notes), Catalog (prompts, agents, workflows as RawWorkflow, skills), Watcher (notify, no symlink following). Symlinked dirs never traversed; project symlinked files skipped. Reload keeps the previous version of a file that now fails and records LoadError{kept_previous}. Duplicate names in one scope are located errors. Editing .harness/ makes the project untrusted (TASK-29), so project hot reload requires re-review; documented. Tests in src/discovery/tests.rs (incl. a real notify watcher test). cargo test -p harness-config: 88 unit + 6 integration tests pass; cargo clippy -p harness-config --all-targets -D warnings clean; cargo fmt check clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Discovery across builtin/global/trusted-project scopes, name shadowing with doctor-ready reports and scoped lookup, hot reload that keeps the last good version with file:line:col errors, notify-based Watcher. Docs: docs/yaml-config.md, doc-3 loader details. Verified with harness-config tests, clippy and fmt.
<!-- SECTION:FINAL_SUMMARY:END -->
