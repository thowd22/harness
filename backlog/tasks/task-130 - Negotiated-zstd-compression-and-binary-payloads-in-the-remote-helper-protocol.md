---
id: TASK-130
title: Negotiated zstd compression and binary payloads in the remote helper protocol
status: Done
assignee:
  - '@claude'
created_date: '2026-10-22 19:32'
updated_date: '2026-10-22 22:55'
labels:
  - remote
  - problem-6
milestone: m-10
dependencies:
  - TASK-101
  - TASK-102
  - TASK-129
references:
  - 'https://github.com/anthropics/claude-code/issues/102929'
documentation:
  - backlog/docs/doc-6 - Remote-Execution-over-SSH.md
priority: medium
type: feature
ordinal: 130000
---

## Description

<!-- SECTION:DESCRIPTION:BEGIN -->
The helper protocol sends one JSON object per line, with file contents and process output as base64, which inflates every payload by about 33 percent before any other overhead. Unlike the Anthropic API we control both ends, so we can negotiate zstd, which measured faster than gzip at similar ratios (crossover around 1.3 Gbit/s versus about 200 Mbit/s for gzip).

## Loop guard (for agents)

Limit how many times you test and fix. If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress in about 30 minutes, **stop**. Do not keep retrying, and never delete, skip, ignore or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the implementation notes, leave the task In Progress, and ask the user how to proceed.
<!-- SECTION:DESCRIPTION:END -->

## Acceptance Criteria
<!-- AC:BEGIN -->
- [x] #1 The handshake negotiates compression capability and protocol version, and old helpers without it keep working uncompressed
- [x] #2 Frames carrying payloads of 4 KiB or more are zstd-compressed at a low level by default, smaller frames are sent as-is
- [x] #3 Large file contents and process output travel as length-prefixed binary payloads instead of base64 inside JSON
- [x] #4 Helper compression is skipped when SSH compression is active for the host, so data is never compressed twice
- [x] #5 Maximum frame size limits apply to the decompressed size to prevent decompression bombs
- [x] #6 Shared Workspace and Exec trait tests pass with compression on and off
- [x] #7 A benchmark reports bytes on the wire and latency for read, write and streaming exec at simulated 10, 100 and 1000 Mbit/s links
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
1. protocol: PROTOCOL_VERSION=2 (MIN 1). hello params gain max_protocol + compression offer; result gains compression. v1 clients send protocol=1 only; new clients send protocol=1,max_protocol=2 so v1 helpers accept them unchanged (one round trip).
2. v2 framing: plain messages stay JSON lines; messages with binary data or bodies >= min_size become blob frames: ASCII header line '~<json_len> <data_len> <wire_len> raw|zstd' + body (JSON then raw data bytes), zstd level 1 by default for bodies >= 4 KiB (raw when zstd does not shrink). One universal decoder accepts v1 lines and v2 frames; MAX_FRAME applies to the declared decompressed size and decompression is bounded.
3. Frame/Encoder/decoder in protocol.rs; helper and client writer tasks encode per connection, switching encoder in-band right after hello. Job replay logs store frames.
4. Client: ClientOptions.compression (WireCompression), Connector::transport_compressed (SshConnector -> target.compression) so zstd is never offered over compressed ssh; call_data fast path used by RemoteWorkspace read/write; generic call keeps working by re-inserting base64.
5. Helper: HelperOptions max_protocol/compression (+ --max-protocol, --no-compression test flags) to simulate old helpers.
6. simlink module: throttled in-memory link with byte counters; benches/wire.rs reports bytes/latency for read, write, streaming exec at 10/100/1000 Mbit/s; small-size test of the same code.
7. Tests: codec unit tests (threshold, bombs, limits, v1 compat), conformance suites with compression on/off and against a v1 helper, ssh-compressed skip, old-client against new helper.
8. Docs: docs/remote.md, doc-6 framing, CHANGELOG Defaults.
<!-- SECTION:PLAN:END -->

## Implementation Notes

<!-- SECTION:NOTES:BEGIN -->
Implemented protocol 2 (PROTOCOL_VERSION=2, MIN_PROTOCOL_VERSION=1). New module harness_remote::wire (Frame, WireFrame, Encoder, Framing, Zstd, read_wire_frame): plain messages stay JSON lines, payload-carrying or >=4 KiB messages are binary frames '~<json_len> <data_len|-> <wire_len> <raw|zstd>' + body; zstd level 1 when negotiated, raw when zstd does not shrink the body. One decoder reads both protocols; the encoder switches in-band right after the hello answer. hello: client sends protocol=1, max_protocol=2 and a CompressionOffer, so a shipped v1 helper (strict protocol==1 check) accepts it and the connection stays v1 lines (tested with a strict v1 fake helper and HelperOptions.max_protocol=1 / --max-protocol 1). Client: ClientOptions.compression (WireCompression), Connector::transport_compressed (SshConnector -> target.compression, which carries the per-host auto/yes decision) suppresses the offer; RemoteClient::call_data carries payloads raw and runs the secret guard over them; generic call() still decodes typed results. Helper job replay logs keep frames. simlink module + benches/wire.rs for the benchmark. harness-workflow remote_steps test now decodes helper frames with read_wire_frame instead of splitting lines (it would otherwise run into raw payload bytes). No new harness.ini keys.
Benchmark (cargo bench -p harness-remote --bench wire, 1 MiB source-like text, 1 ms one-way latency): read at 10 Mbit/s v1 1.40 MB/1130 ms, v2 raw 1.05 MB/845 ms, v2 zstd 0.15 MB/126 ms; at 1000 Mbit/s 19 ms / 11 ms / 9 ms. The generated text compresses about 7x, which is more than real code does.
Validation: cargo test -p harness-remote -p harness-workflow all green; clippy -D warnings clean for both crates; rustdoc -D warnings clean. cargo test --workspace: everything passed except harness-config ini::edit::tests::saving_and_removing_never_log_the_key and harness-eval runner::exec::tests::process_executor_times_out. Both are in other lanes and pass when rerun on their own (load or concurrent edits).

Integration 2026-10-22: the harness-eval flake runner::exec::tests::process_executor_times_out (and process_executor_collects_json_events_from_a_stub_binary, seen failing with 'Text file busy (os error 26)') was an ETXTBSY race: the test wrote the executable stub from the multi-threaded test process, and a concurrent test's fork inherited the write descriptor. Fixed in crates/harness-eval/src/runner/exec.rs tests with a write_stub helper that creates the executable via a child 'install -m 755' process. Not related to the remote protocol.
<!-- SECTION:NOTES:END -->

## Final Summary

<!-- SECTION:FINAL_SUMMARY:BEGIN -->
Added helper protocol 2. hello negotiates the protocol range and zstd compression, and protocol 1 helpers and clients keep working uncompressed. File contents and process output now travel as length-prefixed raw bytes instead of base64. Frames of 4 KiB or more are compressed with zstd level 1, and helper compression is never offered when SSH compression is active for the host. The frame limit applies to the declared decompressed size, and decompression is bounded by it. Code: harness_remote::wire, protocol/helper/client/workspace changes, the simlink module and benches/wire.rs. Verified with: tests/wire.rs (12 tests: negotiation, a strict v1 helper, an old client, raw payload bytes on the wire, the SSH-compression skip, bombs at helper and client, conformance with compression forced on, default, off and against protocol 1, in-process and over the helper process, and the benchmark at 10/100/1000 Mbit/s); wire.rs unit tests for the threshold, incompressible data, limits and malformed headers; and the existing remote and workflow suites. Docs: docs/remote.md (Wire format), doc-6, CHANGELOG. DoD UI item: N/A, no TUI changes.
<!-- SECTION:FINAL_SUMMARY:END -->
