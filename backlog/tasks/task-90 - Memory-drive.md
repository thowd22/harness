---
id: TASK-90
title: Memory drive
status: Done
assignee:
  - '@memory-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:01'
labels:
  - memory
  - problem-4
milestone: m-9
dependencies:
  - TASK-7
documentation:
  - backlog/docs/doc-4 - Memory-System-Design.md
priority: high
type: feature
ordinal: 90000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Every session starting from zero is a major complaint (problem 4). A local, git-backed Markdown drive modeled on Devin Memory gives durable, inspectable memory.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Initializes a git repo at the memory drive path with MEMORY.md, projects, tools and an internal folder
- [x] #2 Project key derives from git remote URL or canonical path
- [x] #3 Notes use one-line bullets with source run link and date, and double-bracket links between notes
- [x] #4 Internal folder is a protected path for agent tools
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
1. drive.rs: Drive::open_or_init (git init -b main, MEMORY.md, projects/, tools/, .harness/ git-ignored, .gitattributes union merge, first commit), NotePath validation, list/read notes.
2. drive/project.rs: project key = slug + sha256 prefix of normalised remote URL, else canonical path.
3. drive/note.rs: Bullet/Note parse+render with (src: harness://run/<id> · date) suffix and [[links]].
4. Drive::protected_paths() for the policy engine; test with harness-policy PolicyEngine.
5. Unit + integration tests, docs/memory.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Git is driven through the git CLI (drive::git::Git) with hooks, signing and system config disabled and a fixed identity, so user git config cannot break writes. .harness/ is git-ignored so worktrees, usage stats and the search index never get committed. Default drive (<data_dir>/memory) is already covered by DataDir::protected_paths(root); Drive::protected_paths() covers a [memory] drive outside the data dir and must be added to PolicyConfig.protected_roots by the agent wiring.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Implemented harness_memory::drive (Drive, NotePath, note format, project keys, git wrapper). Verified by unit tests in drive/, drive/note.rs, drive/project.rs and tests/memory.rs (project_key_uses_remote_then_canonical_path, internal_folder_is_protected_for_agent_tools, tool_search_read_write_forget for note format). cargo test/clippy -D warnings clean. DoD#4 N/A (no TUI). Docs: docs/memory.md.
<!-- SECTION:FINAL_SUMMARY:END -->
