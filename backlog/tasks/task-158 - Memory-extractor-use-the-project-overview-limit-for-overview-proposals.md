---
id: TASK-158
title: 'Memory extractor: use the project/overview limit for overview proposals'
status: Done
assignee:
  - '@claude'
created_date: '2026-10-23 19:43'
updated_date: '2026-10-24 00:41'
labels:
  - memory
dependencies: []
priority: low
ordinal: 165000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
TASK-155 raised the memory tool's per-bullet limit to 1000 characters for project/overview (400 elsewhere; crates/harness-memory/src/tool.rs) and taught the model to answer 'index this codebase' with one compact overview note. The end-of-session memory extractor (crates/harness-memory/src/extract.rs and its guards) still applies the 400-character limit to every proposal, so a valid overview bullet proposed by the extractor is rejected or truncated. Make the extractor (and any shared validation) use the same per-path limits as the tool, from one shared definition so they cannot drift again.
## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Extractor proposals for project/overview up to 1000 characters are accepted; other paths keep 400
- [x] #2 Tool and extractor read the limits from one shared function or constant
- [x] #3 Tests cover both paths at and over the limits; docs/memory.md states the limits once
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
1. guard.rs: add limit_for_path(&str) as the single per-path limit definition (recognises the tool alias project/overview and stored projects/<key>/overview[.md]); limit_for(&NotePath) and is_overview delegate to it; add validate_for_path.
2. extract.rs screen() and dream.rs lesson intake validate with the candidate's path instead of the 400 default.
3. Tests: guard unit tests for both path forms at/over 400 and 1000; extractor screen tests for project/overview at 1000 accepted, 1001 rejected, other paths 400/401.
4. docs/memory.md: state limits once; CHANGELOG Fixed entry.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
guard::limit_for_path(&str) is the single per-path limit definition; it recognises the alias project/overview and stored projects/<key>/overview(.md); limit_for(&NotePath)/is_overview delegate to it. New guard::validate_for_path used by extract::screen and the dream pass lesson intake (dream also applied 400 to every lesson). Tool already used validate_for -> limit_for, so tool and extractor share one function. Extractor system prompt left unchanged (no defaults change; defaults_changelog test passes). DoD#4 N/A: no TUI change. Validation: cargo nextest run -p harness-memory 69/69 pass; clippy -D warnings clean; fmt clean; harness-eval defaults_changelog passes.

Integration validation (2026-10-23, tree including other engineers' in-progress TASK-160/161 work): cargo fmt --all --check, cargo clippy --workspace --all-targets --locked -D warnings, cargo nextest run --workspace --locked --profile ci x3 (1948/1948 passed each run, no flakes), cargo test --workspace --doc, cargo deny check, scripts/hermetic-guard.sh nextest run -p harness -p harness-eval (230 passed, nothing written to HOME/XDG), no .snap.new files, defaults_changelog passes (no defaults changed). CI part of DoD#1 pending until the orchestrator pushes.

Committed in 63eb3bf; hosted CI green on Ubuntu and macOS: https://github.com/thowd22/harness/actions/runs/40045278391
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added guard::limit_for_path (one per-path bullet limit: 1000 for project/overview in alias or stored form, 400 elsewhere) and validate_for_path; the memory tool (via limit_for), the end-of-session extractor screen and the dream pass all use it, so overview proposals up to 1000 chars are accepted. Tests: guard unit test for both path forms at/over 400 and 1000, extractor screen unit test, and an integration test in tests/limits.rs that screens and writes an overview proposal through the tool. docs/memory.md states the limits once in a Bullet limits paragraph; CHANGELOG Fixed entry. Verified with cargo nextest -p harness-memory (69 pass), clippy, fmt.
<!-- SECTION:FINAL_SUMMARY:END -->
