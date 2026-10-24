---
id: TASK-164
title: >-
  Startup update check: notify about a new GitHub release with the install
  command
status: To Do
assignee: []
created_date: '2026-10-24 01:55'
labels:
  - feature
  - tui
dependencies: []
priority: medium
ordinal: 178000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Users don't find out when a new harness release ships, so they keep running old builds. Harness should check GitHub on startup and tell them, with the exact install command ready to copy. The installer is: curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash (README). Releases are published by release-harness.yml as GitHub releases tagged vX.Y.Z; pre-releases (X.Y.Z-rc.N) exist. The check must never slow startup or break offline, sandboxed or headless use, and must not leak anything beyond a plain GET to GitHub.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [ ] #1 On interactive (TUI) startup harness checks the latest GitHub release of thowd22/harness in the background, without delaying the first frame. Offline, rate-limited or failed checks are silent and logged only
- [ ] #2 When the latest release is newer than the running version (semver compare; pre-releases ignored unless the running build is a pre-release), a dialog shows current and new version, a link to the release notes, and the curl install command
- [ ] #3 The dialog has a Copy button that copies the install command to the clipboard (with the existing clipboard/OSC 52 path, so it works over SSH), clickable with the mouse and reachable by keyboard (Tab/Enter)
- [ ] #4 The dialog only closes with Esc; other keys and clicks outside it do not dismiss it
- [ ] #5 The result is cached in the data dir so the check runs at most once per configurable interval (default 24h) and a dismissed version is not shown again until a newer one appears
- [ ] #6 An ini setting turns the check off (for example [general] update_check = false), and headless commands (harness run, wf run, bench, eval) and the test suite never make the request
<!-- AC:END -->

## Definition of Done
<!-- DOD:BEGIN -->
- [ ] #1 Tests: automated tests cover every acceptance criterion (unit and/or integration as appropriate) and pass locally and in CI; no existing test is deleted, skipped, ignored or weakened to get a green run
- [ ] #2 Docs: user-facing behaviour is documented (docs/ and, if the design changed, the relevant backlog doc), public APIs have rustdoc, and any new config keys appear in the harness.ini reference
- [ ] #3 Loop guard respected: no more than 3 fix attempts per failing test or check; if still failing, stop, record attempts, output and hypothesis in implementation notes, and ask the user instead of looping
- [ ] #4 UI interaction: if this task makes anything visible in the TUI, every visible interactive element is clickable with the mouse and fully operable with the keyboard (arrows/Tab to focus, Enter to activate, Esc to back out), with tests for both input paths; tasks with no visible UI mark this N/A
<!-- DOD:END -->
