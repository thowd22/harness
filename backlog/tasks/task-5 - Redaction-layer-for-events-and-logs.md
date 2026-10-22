---
id: TASK-5
title: Redaction layer for events and logs
status: Done
assignee:
  - '@events-engineer'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 17:19'
labels:
  - security
  - events
  - problem-8
milestone: m-0
dependencies:
  - TASK-3
  - TASK-4
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 5000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Tool output can accidentally contain secrets (for example an agent printing env). Because the event log is persisted and shown in the UI, everything must be redacted before it is written or rendered.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 All events pass through a redactor before persistence and before UI delivery
- [x] #2 Redactor masks every secret loaded from harness.ini plus common credential patterns such as API key prefixes, private key blocks and bearer tokens
- [x] #3 Redaction is applied to tool results before they are sent to the model
- [x] #4 Tests include secrets split across streaming chunks
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
1. redact.rs: Redactor built from secret strings (no harness-config dep) + built-in patterns (sk-/gh*/AWS/Slack/Google keys, JWT, bearer, key=value, URL creds, PEM private key blocks incl. unterminated). 2. StreamRedactor holding back a tail so secrets split over chunks are masked. 3. Redact every event in RunRecorder::emit before persistence and broadcast; deltas through per-call StreamRedactor. 4. impl logging::LineRedactor. 5. Tests + docs.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_events::redact: Redactor::new(secrets) (no harness-config dep) + BUILTIN_PATTERNS (PEM incl. unterminated, sk-/rk-, stripe, github, AWS, slack, google, JWT, bearer/basic, UPPER_CASE_SECRET=value, URL creds), redact/redact_json/redact_payload/redact_event, StreamRedactor (holds max(128, longest secret) bytes and any match touching the end), impl logging::LineRedactor. RunRecorder::emit redacts every event before persistence and broadcast; deltas go through a per-(call, channel) StreamRedactor flushed on model_call_finished and finish(). Tests: redact unit tests (every chunk size 1..200 equals whole-text redaction, no partial leaks), tests/store.rs events_are_redacted_before_persistence_and_delivery (disk incl. blobs and UI subscriber contain no secret). REMAINING (outside crates/harness-events, so not done by me): AC#2 needs the binary to collect resolved harness.ini credentials from harness_config::secrets and pass them to Redactor::new (patterns part is done and tested); AC#3 needs the agent loop (TASK-16) to send the redacted tool_result returned by RunRecorder::emit (or recorder.redactor().redact) to the model - the returned event is verified redacted; TASK-6 AC#3 needs crates/harness/src/main.rs to pass Arc<Redactor> via LogConfig::with_redactor. DoD#1 left unchecked until those integrations have tests.

Integration: the binary resolves every [provider.*] credential (wiring::credentials) and builds Redactor::new from them (wiring::redactor) for the run recorder and the log. Tests: crates/harness/tests/headless.rs::json_streams_redacted_events (ini secret masked in --json events), crates/harness-agent/tests/agent_loop.rs::tool_output_is_redacted_before_it_reaches_the_model (AC#3).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Redactor with ini secrets + built-in patterns, applied before persistence, UI delivery, logs and model requests. Verified by harness-events tests, json_streams_redacted_events and tool_output_is_redacted_before_it_reaches_the_model.
<!-- SECTION:FINAL_SUMMARY:END -->
