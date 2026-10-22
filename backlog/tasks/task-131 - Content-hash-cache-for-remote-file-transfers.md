---
id: TASK-131
title: Content-hash cache for remote file transfers
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:32'
updated_date: '2026-10-22 21:58'
labels:
  - remote
  - problem-6
milestone: m-10
dependencies:
  - TASK-130
references:
  - 'https://github.com/anthropics/claude-code/issues/102929'
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: low
type: feature
ordinal: 131000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Agents re-read the same files many times, and each read re-sends the full contents over SSH. Measurements of Claude Code traffic found almost every turn is a pure append, and a prefix-delta cut request bytes by about 100x. The equivalent for us is to avoid resending content the other side already has.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 Client and helper keep a bounded cache of file contents keyed by content hash per session
- [x] #2 A read or write whose content hash the other side already holds transfers only the hash
- [x] #3 Cache misses, evictions and hash mismatches fall back to a full transfer without errors
- [x] #4 The cache never changes what tools return, proven by tests comparing cached and uncached results
- [x] #5 Bytes saved are recorded in remote operation events
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
1. New harness_remote::cache module: byte-bounded LRU ContentCache keyed by sha256 (harness_tools::tracker::content_hash), verified on lookup, plus a hash-only HashSet mirror.
2. Protocol: HelloResult.content_cache (helper capacity), ReadParams{path, have}, DataResult{hash, cached}, WriteParams{hash, cached}, codes CACHE_MISS / HASH_MISMATCH.
3. Helper: per-session cache in Inner (survives reattach), fill on read/write, answer cached reads with the hash only, write cached blobs by hash, refuse missing/mismatching hashes; HelperOptions.content_cache + --content-cache flag.
4. Client: per-session cache + path->hash map + mirror of what the helper holds; RemoteClient::read_file/write_file with fallback to a full transfer on miss/eviction/mismatch; one audit event per logical op with bytes_saved. ClientOptions.content_cache.
5. Events: AuditEvent.bytes_saved and RemoteOp.bytes_saved (serde default, skipped when 0).
6. RemoteWorkspace uses read_file/write_file.
7. Tests: unit (cache, helper), integration (hash-only on the wire via SimLink stats, fallbacks after helper restart/eviction/mismatch, cached vs uncached tool results equal, bytes_saved in run log).
8. Docs: doc-6, docs/ remote page, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented harness_remote::cache (ContentCache: byte-bounded LRU keyed by sha256, verified on lookup; PeerCache: client's model of the helper cache). Protocol: HelloResult.content_cache, ReadParams{have}, DataResult{hash,cached}, WriteParams{hash,cached}, codes CACHE_MISS/HASH_MISMATCH (all serde-optional, protocol 1 and old peers unaffected; writes by hash only go to helpers that advertise a cache). Helper: per-session cache in Inner (survives reattach), --content-cache BYTES. Client: RemoteClient::read_file/write_file, ClientOptions.content_cache/with_content_cache, cache_stats/clear_cache; one audit event per logical op with bytes_saved even when a fallback needed a second RPC. AuditEvent.bytes_saved and EventKind::RemoteOp.bytes_saved (serde default, omitted when 0; additive edit in harness-events). RemoteWorkspace read/write/replace_atomic go through the cache.
Decision: tests/wire.rs connect() and simlink::bench now run with the content cache off (with_content_cache(0) / opts.content_cache = 0) because they measure framing of full transfers and their read-after-write / write-of-just-read-content would otherwise become hash-only; no assertion was changed. Cache coverage lives in tests/content_cache.rs (incl. protocol 1 and the real helper process via the existing over_process conformance test, which runs with the cache on).
Validation: cargo test -p harness-remote (all suites pass, 12 new in content_cache.rs + unit tests in cache.rs, helper args, protocol), cargo test --workspace green, cargo clippy -p harness-remote -p harness-events --all-targets -D warnings clean, rustfmt clean, RUSTDOCFLAGS=-D warnings cargo doc -p harness-remote clean. No live SSH verification was done (offline tests only).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added a per-session content-hash cache to remote file transfers. Client and helper each keep a bounded (64 MiB default, LRU) sha256-keyed cache; repeat reads, reads after writes and writes of content the helper holds send only the hash. Helper misses (CACHE_MISS), evictions, corrupt copies, hash mismatches (HASH_MISMATCH) and cacheless/old helpers fall back to full transfers without errors. remote_op events carry bytes_saved. Verified with tests/content_cache.rs (wire byte counts via SimLink, fallbacks, bounded caches, protocol 1, cached vs uncached tool-session equality, run-log bytes_saved), cache.rs unit tests and the full workspace suite. Docs: docs/remote.md (Content cache), doc-6, docs/workflows.md, CHANGELOG. DoD UI: N/A (no TUI change).
<!-- SECTION:FINAL_SUMMARY:END -->
