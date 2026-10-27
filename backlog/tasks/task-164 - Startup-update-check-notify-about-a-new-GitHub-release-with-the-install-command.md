---
id: TASK-164
title: >-
  Startup update check: notify about a new GitHub release with the install
  command
status: In Progress
assignee:
  - '@claude'
created_date: '2026-10-24 01:55'
updated_date: '2026-10-27 17:10'
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
- [x] #1 On interactive (TUI) startup harness checks the latest GitHub release of thowd22/harness in the background, without delaying the first frame. Offline, rate-limited or failed checks are silent and logged only
- [x] #2 When the latest release is newer than the running version (semver compare; pre-releases ignored unless the running build is a pre-release), a dialog shows current and new version, a link to the release notes, and the curl install command
- [x] #3 The dialog has a Copy button that copies the install command to the clipboard (with the existing clipboard/OSC 52 path, so it works over SSH), clickable with the mouse and reachable by keyboard (Tab/Enter)
- [x] #4 The dialog only closes with Esc; other keys and clicks outside it do not dismiss it
- [x] #5 The result is cached in the data dir so the check runs at most once per configurable interval (default 24h) and a dismissed version is not shown again until a newer one appears
- [x] #6 An ini setting turns the check off (for example [general] update_check = false), and headless commands (harness run, wf run, bench, eval) and the test suite never make the request
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
1. Config: [general] update_check (on/off, default on) and update_check_interval (duration, default 24h); DataDir::update_check() -> <data_dir>/update-check.json; reference.ini, doc-2, tests.
2. Core module crates/harness/src/update.rs: semver parse/compare (pre-release rules), pick the newest eligible release from GET {api}/repos/thowd22/harness/releases (plain GET via harness_providers::http::HttpClient, no auth headers, short timeouts), JSON cache (checked_at, latest, dismissed) so the request runs at most once per interval, dismissed-version filter, failures logged via tracing only. API base injectable (parameter; HARNESS_UPDATE_API env for binary tests).
3. TUI widget crates/harness-tui/src/update.rs: UpdateModal (current/new version, release-notes link as OSC 8 Hyperlink, install command, [ copy install command ] and [ copy link ] buttons via Interaction/HitMap, Tab/Enter, scroll for small terminals). Closes only on Esc -> UiRequest::DismissUpdate(version); clicks outside and other keys ignored.
4. App wiring: CoreEvent::UpdateAvailable, routing (keys/mouse/paste) after sign-in and login modals, render, Copy via existing Effect::Copy (clipboard/OSC 52).
5. Driver: spawn background check from tui::run only (not Driver::start, not headless/run/wf/bench/eval, not ui-demo); DismissUpdate writes the cache.
6. Tests: unit (semver, pick, cache interval, dismissed), mock-server driver tests (dialog event, failures silent, interval, disabled setting), modal keyboard+mouse tests, app tests, layout matrix case, binary tests: harness run makes no request, test helper points HARNESS_UPDATE_API at a closed port.
7. Docs: docs/tui.md (or new docs/update-check.md), data-dir.md, harness.ini reference, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented: [general] update_check (default on) + update_check_interval (default 24h); DataDir::update_check() = <data_dir>/update-check.json; crates/harness/src/update.rs (semver, newest stable/any release, JSON cache with checked_at/stable/any/dismissed, plain GET via harness_providers HttpClient, failures logged under harness::update); crates/harness-tui/src/update.rs UpdateModal (Esc-only close, Tab/arrows/Enter, clickable buttons, OSC 8 link, scroll); app wiring CoreEvent::UpdateAvailable / UiRequest::DismissUpdate; Driver::spawn_update_check called only from tui::run (ui-demo sets update_check = false). HARNESS_UPDATE_API env overrides the API base; tests/common hermetic() points it at 127.0.0.1:9 so binary tests never reach GitHub. Docs: docs/update-check.md, data-dir.md, tui.md, guide/harness-ini.md, guide/install.md, doc-2, CHANGELOG (Added + Defaults entry with new fingerprint d38458c654e8).

Validation (local): cargo fmt --check ok; cargo clippy --workspace --all-targets -D warnings ok; cargo nextest run --workspace --locked --profile ci: 2118 passed, 10 skipped; cargo test --workspace --doc ok; scripts/hermetic-guard.sh ok (278 passed, nothing written to HOME/XDG); scripts/bench-guard.sh ok (3.25 ms/call, 10.7 ms session start); defaults_changelog ok. No fix loops needed beyond adjusting two new test assertions. DoD #1 left unchecked until CI runs.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Startup update check for the interactive TUI. After the session starts, Driver::spawn_update_check (called only from tui::run) asks GitHub in a background task with one plain GET of /repos/thowd22/harness/releases (harness_providers HttpClient, no credentials), picks the newest stable release (or any, for pre-release builds) by semver, caches checked_at/stable/any/dismissed in <data_dir>/update-check.json so GitHub is asked at most once per [general] update_check_interval (default 24h; failed checks count), and logs failures only. A newer, not-dismissed release opens a new UpdateModal (harness-tui/src/update.rs): current/new version, OSC 8 release-notes link, install command, [ copy install command ] and [ copy link ] via the app clipboard (OSC 52), Tab/arrows/Enter and mouse; closes only with Esc, which sends UiRequest::DismissUpdate and records the version. [general] update_check = off disables it; headless commands and ui-demo never call it; HARNESS_UPDATE_API overrides the API base and the binary-test helper points it at a closed local port. Verified by unit tests (crates/harness/src/update/tests.rs), driver tests (tui/tests/update.rs), modal + app tests and a styled snapshot (harness-tui/src/update/tests.rs), a layout-matrix case, and pty end-to-end tests (crates/harness/tests/update_check.rs) against a local mock API; full workspace suite, clippy, fmt, doctests, hermetic and bench guards green locally. CI not yet run.
<!-- SECTION:FINAL_SUMMARY:END -->
