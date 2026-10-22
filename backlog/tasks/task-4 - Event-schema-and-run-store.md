---
id: TASK-4
title: Event schema and run store
status: Done
assignee:
  - '@events-engineer'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:08'
labels:
  - core
  - events
milestone: m-0
dependencies:
  - TASK-1
documentation:
  - backlog/docs/doc-1 - Architecture-and-Problem-Map.md
priority: high
type: feature
ordinal: 4000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Runs are event-sourced (ADR-2). The map view, history, replay, resume, evals and debugging all derive from one append-only log, so the schema is the backbone of the system and must be versioned from the start.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Typed, versioned RunEvent schema covering run, node, model call, delta, tool call, tool result, approval, file change, verify, memory, cost and error events
- [x] #2 Events carry run id, node id, parent node id, monotonic sequence and timestamp
- [x] #3 Async buffered JSONL writer per run under data_dir/runs/run-id with a content-addressed blob store for large payloads
- [x] #4 Reader can replay a run from disk, tolerating a truncated final line after a crash
- [x] #5 Schema version is written in the run header and old versions are rejected or migrated explicitly
- [x] #6 Tests cover round-trip, crash truncation and blob dedupe
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
1. schema.rs: SCHEMA_VERSION, RunId/NodeId, RunHeader, RunEvent envelope (run_id, node_id, parent_node_id, seq, ts) + EventKind covering run/node/model call/delta/tool call/result/approval/file change/verify/memory/cost/error; Payload (text/json/blob). 2. blobs.rs: per-run sha256 content-addressed store with atomic writes and dedupe. 3. store.rs: RunRecorder (sequence assignment, redaction hook, blob spill, broadcast to UI) feeding an async tokio BufWriter task; RunReader replays and tolerates a truncated final line; header version check + explicit migration hook. 4. Tests + docs/events.md.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented schema.rs (SCHEMA_VERSION=1, RunHeader, RunEvent envelope, EventKind with 19 variants, Payload text/json/blob, migrate_event), blobs.rs (per-run sha256 store, atomic temp+rename, dedupe, spill/resolve), store.rs (RunRecorder: seq+ts stamping, parent auto-fill from node_started, redaction, blob spill, broadcast subscribe, tokio BufWriter task flushing per batch; read_run tolerating torn final line incl. invalid UTF-8, rejecting mid-file corruption, out-of-order seq and unsupported versions; list_runs). Tests: schema unit tests, blobs unit tests, tests/store.rs (9 tests: round trip, truncation, corruption, version rejection, blob dedupe, redaction). DoD#4 N/A (no UI). Docs: docs/events.md. Note for TASK-19 (resume): appending to a run with a torn last line must first truncate to the last complete line, otherwise that line becomes mid-file corruption.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Versioned event schema, per-run JSONL store with async buffered writer and content-addressed blobs, crash-tolerant reader with explicit version check. Verified with cargo test -p harness-events (schema/blobs unit tests + tests/store.rs) and clippy -D warnings.
<!-- SECTION:FINAL_SUMMARY:END -->
