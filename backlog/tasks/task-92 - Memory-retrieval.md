---
id: TASK-92
title: Memory retrieval
status: Done
assignee:
  - '@memory-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 16:02'
labels:
  - memory
  - problem-4
milestone: m-9
dependencies:
  - TASK-90
  - TASK-83
documentation:
  - backlog/docs/doc-4 - Memory-System-Design.md
priority: high
type: feature
ordinal: 92000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Loading everything is expensive and noisy; Devin loads an index and searches on demand.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 MEMORY.md and the current project's index are loaded at session start within index_max_tokens
- [x] #2 BM25 search over notes using an incremental index
- [x] #3 Reads are recorded per note and run for Dreaming
- [x] #4 Index is frozen for the session for cache stability
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
1. index/session.rs: SessionIndex::load(checkout, project_key, index_max_tokens) = MEMORY.md + project notes section, capped at line boundaries with a truncation notice; frozen value with sha256.
2. index/bm25.rs: SearchIndex with per-note content hash, incremental update(), BM25 search with snippets, JSON cache in .harness/search-index.json.
3. index/usage.rs: UsageLog append-only jsonl of (note, run, ts, kind) with per-note summary for Dreaming.
4. Tool search/read record usage. Tests.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Used a small built-in BM25 instead of tantivy (notes are tiny; avoids a heavy dependency); doc-4 updated. The frozen index is a value computed once; harness-agent must call SessionIndex::load once per session and place text() in the stable prefix after tools (TASK-83 owns the layout). Tokens estimated as bytes/4.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Implemented harness_memory::index (SessionIndex, SearchIndex BM25, UsageLog). Verified by unit tests in index/ and tests/memory.rs session_index_is_capped_frozen_and_reads_are_recorded and search_index_is_cached_and_incremental. clippy clean. DoD#4 N/A.
<!-- SECTION:FINAL_SUMMARY:END -->
