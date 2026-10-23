---
id: TASK-147
title: Tests must never write to the user's real data or config directories
status: To Do
assignee: []
created_date: '2026-10-23 06:44'
labels:
  - testing
  - eval
  - security
  - problem-8
milestone: m-11
dependencies: []
priority: high
type: bug
ordinal: 151000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Running the test suite on a developer machine left an eval session (/tmp/harness-eval-long-context-…, 'In data/settings.txt…') in the user's real ~/.local/share/harness sessions list: some harness-eval tests run the real binary through the process executor without isolating its data dir, HOME or XDG paths. Tests must be hermetic so they cannot pollute or read a user's sessions, memory, config or credentials.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 Every test that runs the harness binary or creates a data dir points HARNESS_CONFIG, XDG_CONFIG_HOME, XDG_DATA_HOME, XDG_CACHE_HOME and HOME at a temp dir
- [ ] #2 The eval process executor isolates each attempt's data dir by default, and documents how to opt in to a shared one
- [ ] #3 A guard test runs the full harness-eval and harness integration suites with HOME and XDG dirs pointed at a sentinel directory and fails if anything is created there
- [ ] #4 The leftover eval session in the user's real sessions list is explained in the notes (it was created by a local test run), and the CHANGELOG notes the fix
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
