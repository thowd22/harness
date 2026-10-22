---
id: TASK-3
title: Credential handling in harness.ini
status: Done
assignee:
  - '@config-ini'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:09'
labels:
  - config
  - security
  - problem-8
milestone: m-0
dependencies:
  - TASK-2
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 3000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Credentials live in harness.ini by user request, which makes the file sensitive. Secrets must never be printed, logged, sent to the model, or inherited by subprocesses.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Provider keys resolve from api_key, then api_key_cmd, then api_key_env, first non-empty wins
- [x] #2 Secrets are wrapped in a type whose Debug and Display output is redacted and whose memory is zeroized on drop
- [x] #3 Loading secrets from a file that is group or world readable is refused with an explanation of how to fix permissions
- [x] #4 api_key_cmd has a timeout and its stderr is not echoed into logs
- [x] #5 Tests prove secrets do not appear in Debug output, logs or serialized config
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
1. secrets::Secret over Zeroizing<String>; Debug/Display/Serialize print [redacted].
2. secrets::resolve with precedence inline > cmd > env, first non-empty wins.
3. api_key_cmd via sh -c, stdin null, stderr null, timeout with kill.
4. ini::load_file refuses inline secrets in group/world-readable files with chmod 600 hint.
5. Leak tests over Debug, JSON, errors and captured tracing logs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
crates/harness-config/src/secrets.rs; permission check in ini/load.rs. Command failure falls back to api_key_env if it yields a value, else the command error (status only, never stderr) is returned. Config::secret_env_vars() lists api_key_env names for subprocess env scrubbing (tools crate must env_remove them). Only inline api_key triggers the permission refusal (cmd/env names are not secrets). Tests: secrets::tests (precedence, timeout, stderr suppression, mode bits), tests/secrets_no_leak.rs (Debug, serde_json, errors, tracing logs captured at TRACE). Docs: docs/config.md Credentials section. DoD#4 N/A. Verified: cargo test -p harness-config (42 unit + 5 integration + 1 doctest pass), cargo clippy -p harness-config --all-targets -- -D warnings clean, RUSTDOCFLAGS=-D warnings cargo doc clean.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Secret type (zeroizing, redacted Debug/Display/Serialize), precedence resolver, timed api_key_cmd with suppressed stderr, insecure-permission refusal; verified by unit + integration leak tests.
<!-- SECTION:FINAL_SUMMARY:END -->
