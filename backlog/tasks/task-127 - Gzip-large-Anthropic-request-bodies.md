---
id: TASK-127
title: Gzip large Anthropic request bodies
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:32'
updated_date: '2026-10-22 20:23'
labels:
  - provider
  - problem-1
  - problem-6
milestone: m-1
dependencies:
  - TASK-10
  - TASK-11
references:
  - >-
    https://aicoder.com/news/news-20261017-claude-code-3x-faster-first-token-gzip-cloud-prewarm
  - 'https://github.com/anthropics/claude-code/issues/90457'
  - 'https://github.com/NousResearch/hermes-agent/pull/142050'
documentation:
  - backlog/docs/doc-2 - harness.ini-Reference.md
priority: high
type: feature
ordinal: 127000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
Every turn re-sends the whole conversation, often hundreds of KB to over 1 MB, and the upload dominates time to first token on slower uplinks. Claude Code 2.1.300 (Oct 2026) started gzipping these bodies and Anthropic reported p90 wait before first token falling from 4.07s to 1.33s. Our Anthropic provider currently sends uncompressed JSON. The API is observed (not officially documented) to accept only Content-Encoding gzip and to reject zstd, br and deflate with 400, so gzip is the only option here and we need a safe fallback. Compression changes bytes on the wire only, not billed tokens or the prompt-cache key.

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 POST /v1/messages and /v1/messages/count_tokens bodies of 4 KiB or more to api.anthropic.com on the default port are sent gzip-encoded (single member, mtime 0, level configurable with a low default such as 3)
- [x] #2 Custom base_url, non-default ports and requests through a proxy or custom CA are never compressed
- [x] #3 If the server rejects the encoding (415, a 400 with request_body_encoding_unsupported or an invalid-JSON error, or a 400/403/415 from a non-Anthropic hop), the raw body is resent once and gzip is disabled for the rest of the process; a second rejection is returned without further retries
- [x] #4 A rejection while streaming closes the response before the fallback so no connection is leaked
- [x] #5 provider.anthropic gzip_requests = auto or off in harness.ini controls it, and the HARNESS_GZIP_REQUEST_BODIES=0 env var disables it
- [x] #6 The decompressed body is byte-identical to the uncompressed body, proven by a wire-capture test, so golden request snapshots and cache keys are unchanged
- [x] #7 Model call events record raw and compressed sizes, and the request inspector shows both
- [x] #8 A benchmark against a local mock server reports upload time saved at simulated 10, 100 and 1000 Mbit/s links
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
1. http.rs: RequestCompression (level, min_bytes 4096, path allowlist, process-wide GzipLatch) on HttpConfig; serialize body with serde_json::to_vec (same bytes as reqwest json), gzip single member mtime 0 via flate2 when eligible; classify rejections (415; 400 request_body_encoding_unsupported/invalid JSON; 400/403 from a non-Anthropic hop = no request-id header and no Anthropic error JSON), drop the rejected response, trip the latch, resend raw once.
2. anthropic.rs: GzipRequests {Auto, Off, Always(tests)} + gzip_level on AnthropicConfig; auto only for https://api.anthropic.com on default port with no proxy env (HTTPS_PROXY/ALL_PROXY) and no custom CA env (SSL_CERT_FILE/SSL_CERT_DIR); HARNESS_GZIP_REQUEST_BODIES=0 disables; add count_tokens (POST /v1/messages/count_tokens).
3. Wire sizes: Notice::RequestBody {provider, raw_bytes, wire_bytes, encoding} emitted per attempt as a harness opaque block, so model_call_finished.response records it; inspector reads it and shows raw and wire sizes.
4. harness-config: [provider.anthropic] gzip_requests = auto|off, gzip_level = 1..9; wiring in crates/harness/src/wiring.rs.
5. Tests: wire-capture byte identity, eligibility matrix, fallback cases incl. streaming rejection closing the connection, config parsing, inspector line; example/bench reporting upload time saved at 10/100/1000 Mbit/s against a throttled local mock server.
6. Docs: providers-anthropic.md, providers.md, doc-2 ini reference, guide harness-ini, CHANGELOG.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented gzip in the shared HTTP layer (crates/harness-providers/src/http.rs + http/compression.rs): RequestCompression on HttpConfig, body serialized once with serde_json::to_vec (same bytes as reqwest .json()), single-member gzip with mtime 0, per-path allowlist and 4 KiB threshold, process-wide GzipLatch. Rejections (415; 400 request_body_encoding_unsupported or 'body is not valid JSON'; 400/403 from a non-Anthropic hop = no request-id header and no Anthropic error JSON) drop the response (415 unread; others read up to complete JSON / 64 KiB / 1 s idle) before one raw resend; latch trips.
Anthropic provider: GzipRequests {Auto, Off, Always(tests only)}, gzip_level (default 3), gzip_latch; gzip_eligible() requires https://api.anthropic.com default port, no path, no HTTPS_PROXY/ALL_PROXY, no SSL_CERT_FILE/SSL_CERT_DIR; HARNESS_GZIP_REQUEST_BODIES=0 disables. Added count_tokens(). Each response gets a harness Notice::RequestBody block (raw/wire/encoding) inserted just before MessageStop so retry discard semantics are unchanged; it lands in model_call_finished.response and the inspector shows it.
Config: [provider.anthropic] gzip_requests = auto|off, gzip_level = 1..9, wired via wiring::anthropic_config.
Golden request snapshots unchanged (cargo test -p harness-eval --test golden_requests: 6 passed). defaults_changelog currently fails with fingerprint 9615b4d09acd, but Config::defaults() has no providers so this change does not affect it; the drift comes from another lane's builtin/default changes.
No live verification: no Anthropic key is available; all tests use local mock servers.

Validation: cargo test -p harness-providers -p harness-config (all pass, incl. 14 in tests/gzip_requests.rs and new unit tests), cargo test -p harness-tui --lib inspector (11 pass, incl. request_tab_shows_raw_and_wire_body_size), cargo test -p harness --bin harness (22 pass), cargo test -p harness-eval --test golden_requests (6 pass, snapshots unchanged), clippy -D warnings clean for harness-providers, harness-config, harness-tui, harness. DoD UI: the inspector size line is plain text with no interactive elements, so N/A. Benchmark (debug build, 256 KiB setting, 286,255 -> 47,303 bytes): 10 Mbit/s 243 -> 62 ms, 100 Mbit/s 34 -> 27 ms, 1000 Mbit/s 13 -> 23 ms.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
The Anthropic provider now gzips request bodies of 4 KiB or more for /v1/messages and /v1/messages/count_tokens. Compression happens in the shared HTTP layer as one gzip member with mtime 0, level 3 by default. It is on only for https://api.anthropic.com on the default port with no proxy or custom CA configured. If the server rejects the encoding, the response is closed, the raw body is sent once more, and gzip stays off for the rest of the process. It is controlled by [provider.anthropic] gzip_requests = auto|off and gzip_level, or by HARNESS_GZIP_REQUEST_BODIES=0. Each response records its raw and wire size as a harness request_body notice, which the request inspector shows. Wire-capture tests prove the decompressed body is byte-identical, and the golden snapshots did not change. A throttled mock-server benchmark reports upload time at 10, 100 and 1000 Mbit/s. All tests are offline; nothing was verified against the live API because no Anthropic key is available.
<!-- SECTION:FINAL_SUMMARY:END -->
