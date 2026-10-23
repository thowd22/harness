---
id: TASK-139
title: curl | bash install script
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 20:27'
updated_date: '2026-10-23 04:09'
labels:
  - infra
  - release
  - security
milestone: m-11
dependencies:
  - TASK-138
references:
  - 'https://github.com/thowd22/harness'
priority: high
type: feature
ordinal: 139000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Installing from source needs a Rust toolchain. Users expect a one-line install that downloads the right prebuilt binary from GitHub Releases. Because piping a script into bash runs it with the user's privileges, the script must be short, readable and verify what it downloads.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash installs the latest release; HARNESS_VERSION pins a version and HARNESS_INSTALL_DIR overrides the default ~/.local/bin
- [x] #2 Detects linux x86_64 and aarch64 and macOS arm64 and x86_64, matches the release asset names from the release workflow, and fails with a clear message (and the cargo install fallback) on unsupported platforms
- [x] #3 Verifies the sha256 of the download before installing and aborts on mismatch; uses only curl or wget, tar or plain binaries, and sha256sum or shasum
- [x] #4 Never uses sudo, installs atomically, is safe to re-run for upgrades, and prints a PATH hint when the install dir is not on PATH
- [x] #5 The whole script is wrapped in a main function called on the last line, so a truncated download cannot run a partial script
- [x] #6 shellcheck passes, and a CI job runs the script against a local fake release server for each supported platform name
- [x] #7 The install one-liner is the first thing in README.md after the title and description, with a link to read the script first and uninstall instructions
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
1. install.sh (POSIX sh, main wrapper): detect platform -> release target triple, resolve latest via GitHub API or HARNESS_VERSION, download harness-VERSION-TARGET.tar.gz + .sha256 (curl or wget), verify sha256, extract, atomic mv into HARNESS_INSTALL_DIR (default ~/.local/bin), PATH hint.
2. Test-only overrides HARNESS_RELEASES_URL / HARNESS_API_URL for the fake server.
3. tests/install-test.sh + tests/fixtures/install (fake uname/sysctl): builds assets with scripts/release-package.sh for every target in release-harness.yml, serves them with python3 -m http.server, covers every AC.
4. .github/workflows/install-test.yml: shellcheck + tests on ubuntu and macOS.
5. docs/guide/install.md and README install section.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented install.sh (114 lines, POSIX sh, main() called on last line), tests/install-test.sh + tests/fixtures/install/bin/{uname,sysctl}, .github/workflows/install-test.yml (shellcheck job + fake-release job on ubuntu-latest and macos-latest), docs/guide/install.md, README install section right after the tagline.
Asset names: harness-X.Y.Z-<target>.tar.gz (+ .sha256, sha256sum format) under releases/download/vX.Y.Z/, top-level dir harness-X.Y.Z-<target>/harness; targets x86_64/aarch64-unknown-linux-musl, aarch64/x86_64-apple-darwin, matching release-harness.yml + scripts/release-package.sh as of now. The test reads the target list from release-harness.yml and packs fake assets with scripts/release-package.sh, so drift breaks the test.
Latest version: GitHub API releases/latest tag_name. Test-only overrides: HARNESS_RELEASES_URL (download base) and HARNESS_API_URL (latest lookup).
Local: shellcheck 0.10.0 (installed to ~/.local/bin) clean; tests/install-test.sh 30/30 ok (every platform name, Rosetta, pin, default dir, unsupported platforms, sha mismatch, missing release, wget-only, shasum-only, no downloader, no sudo, upgrade/no temp files, PATH hint, every truncated prefix).
Fix attempts: 1 on the truncation test (byte cuts -1/-5/-6 leave a complete main call, so excluded with a comment; cuts 2-4,7-12 and every line prefix still must run nothing).
Open: AC1 live install from a real GitHub release cannot be tested until the first release exists; AC6/DoD1 CI job not yet run on GitHub (no push allowed). CHANGELOG not updated (not in my owned files).

Review fixes (both findings confirmed real, both fixed):
1. HTTPS-only: fetch() refuses any non-https:// URL unless HARNESS_INSTALL_TEST=1; curl runs --proto '=https' --tlsv1.2 normally, and in test mode --proto '=https,http' --proto-redir '=https' (redirects always HTTPS). wget cannot restrict redirect protocols, so wget_https() runs wget --max-redirect=0 -S, follows Location by hand (max 5 hops, absolute or /-relative) and dies on any non-https hop.
2. Overrides: HARNESS_RELEASES_URL/HARNESS_API_URL are resolved once in main via override(); set without HARNESS_INSTALL_TEST=1 -> die naming the var and value, nothing downloaded; honoured -> stderr 'harness-install: WARNING: VAR is set; downloading from URL (test override, not GitHub)'. 'downloading' line now prints the full URL.
Tests: tests/install-test.sh sets HARNESS_INSTALL_TEST=1 and adds: static check of the normal curl flags, each override refused without opt-in, warnings on stderr + full URL on stdout, curl and wget each refuse a redirect to plain http (python http.server 301s $base/download? to http). Mutation check: dropping --proto-redir or the wget redirect check makes those two tests fail. 36/36 ok locally, shellcheck clean.
Fix attempts: 1 (new test set both overrides at once; install.sh used tail/tr, not in the minimal tool PATH -> replaced with sed|head).
Limits: the production https path and real GitHub redirect (to objects.githubusercontent.com, https) cannot be exercised locally without TLS; wget TLS minimum is left to wget defaults (no portable flag). docs/guide/install.md updated.

Live: curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash installed harness 0.1.0 (3a75f3f7cddb) to ~/.local/bin from https://github.com/thowd22/harness/releases/tag/v0.1.0 with the sha256 checked; HARNESS_VERSION=0.1.0 with a custom HARNESS_INSTALL_DIR installed the same build and printed the PATH hint. The install-test CI workflow (shellcheck plus fake-release installs on ubuntu and macos) is green on main. Docs: README install section and docs/guide/install.md (HTTPS-only, test overrides, uninstall).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
One-line installer for GitHub releases, HTTPS-only with sha256 verification; verified by a real install of v0.1.0 and the install-test CI workflow.
<!-- SECTION:FINAL_SUMMARY:END -->
