---
id: TASK-10
title: 'HTTP, SSE streaming and retry layer'
status: Done
assignee:
  - '@provider-core'
created_date: '2026-10-22 14:30'
updated_date: '2026-10-22 15:09'
labels:
  - provider
  - problem-1
milestone: m-1
dependencies:
  - TASK-8
priority: high
type: feature
ordinal: 10000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Flaky networks, rate limits and overloaded APIs are a leading cause of broken agent runs. One shared, well-tested transport keeps all three providers equally robust.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 reqwest with rustls, configurable timeouts and base_url per provider
- [x] #2 SSE parser handles multi-line data, comments, partial chunks and keep-alives
- [x] #3 Retries with jittered exponential backoff honour retry-after for 429, 5xx and overloaded responses, with a retry cap
- [x] #4 Stream stall detection aborts and retries a call that receives no bytes for a configurable interval
- [x] #5 Calls are cancellable and every retry emits an event
- [x] #6 Tests use a local mock server for each failure mode
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
1. sse.rs: incremental SseParser (multi-line data, comments, CR/LF/CRLF, partial chunks incl split UTF-8) + stream adapter.
2. retry.rs: RetryPolicy (jittered exp backoff, cap), Retry-After/retry-after-ms parsing, RetryReason classification, RetryEvent, retry_stream/retry_call combinators with cancellation.
3. http.rs: HttpConfig (base_url, connect/request/stall timeouts, retry), HttpClient on reqwest+rustls, single-attempt open with status/error mapping, stall-detecting byte stream, post_sse/post_json with retries.
4. Integration tests against a local tokio TCP mock server for 429/5xx/529/stall/cancel/partial chunks.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
http.rs: HttpConfig{base_url, connect_timeout, request_timeout, stall_timeout, retry, default_headers}, HttpClient (reqwest+rustls) with open (single attempt, status->typed error, retry-after parsing, stall-guarded bytes), post_sse (retrying SSE stream), post_json (retrying, whole-request timeout); stall_guard; error_message. sse.rs: incremental SseParser (multi-line data, comments, CR/LF/CRLF incl split CRLF, split UTF-8, BOM, lenient final event) + sse_stream adapter. retry.rs: RetryPolicy (jittered exp backoff, cap, max_retry_after), parse_retry_after (retry-after-ms, seconds, HTTP date), RetryReason, RetryEvent{discard_partial}, retry_call, retry_stream (retries open and mid-stream errors, cancellable). Mock server tests (tests/http_transport.rs, raw tokio TCP): partial chunks/comments, 429+retry-after, 500/529/503, retry cap, permanent 400/401, mid-stream stall, stall before headers, connection refused, cancel, post_json retry, request timeout, base_url/headers. Ran 5x with no flakes. Added tokio-util to [workspace.dependencies] for CancellationToken. harness.ini keys proposed in docs/providers.md (connect_timeout, request_timeout, stall_timeout, max_retries under [provider.X]); not parsed yet, harness-config owns that. Validation: cargo test -p harness-providers (40 unit + 12 mock-server transport + 1 YAML fixture test, all pass); cargo clippy -p harness-providers --all-targets -- -D warnings clean; cargo doc clean. No UI (DoD#4 N/A).
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Shared HTTP/SSE/retry transport with stall detection, cancellation and retry events; verified with local mock-server tests for each failure mode.
<!-- SECTION:FINAL_SUMMARY:END -->
