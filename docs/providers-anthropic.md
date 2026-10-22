# Anthropic provider

`harness_providers::anthropic::AnthropicProvider` calls Claude through the
Messages API (`POST /v1/messages`). Every call is streamed.

```ini
[provider.anthropic]
api_key_env = ANTHROPIC_API_KEY      ; or api_key / api_key_cmd
; base_url = https://api.anthropic.com
; gzip_requests = auto                ; or off
; gzip_level = 3                      ; 1 (fastest) to 9 (smallest)
```

The key is sent only in a sensitive `x-api-key` header. It never appears in
logs, `Debug` output or error messages. Every request carries the header
`anthropic-version: 2026-10-21`.

## Streaming

Text, thinking, signatures and tool-call arguments are forwarded as they
arrive:

| Arrives as | Becomes |
|---|---|
| Text deltas | `TextDelta` |
| Thinking deltas | `ReasoningDelta`, shown live while the model thinks |
| Signature deltas | `ReasoningSignature` |
| Tool argument fragments | `ToolInputDelta` |

Parallel tool calls arrive as several `tool_use` blocks in one message.

Blocks Harness cannot interpret, such as `redacted_thinking`, server tool
blocks and future block types, are buffered. Each one is emitted once, when
its block ends, as an `Opaque` block for the `anthropic` provider.

Retries:

- An `overloaded_error`, `rate_limit_error` or `api_error` event in the middle
  of a stream is retried like the matching HTTP status.
- A stream that ends before `message_stop` is retried as a connection error.
- Output that was already streamed from the failed attempt is discarded
  (`Retry { discard_partial: true }`).

## Request compression

Every turn re-sends the whole conversation, often hundreds of KB, and on a
slow uplink the upload dominates the time to the first token. Request
bodies of 4 KiB or more for `POST /v1/messages` and
`POST /v1/messages/count_tokens` are therefore sent with
`Content-Encoding: gzip` (one gzip member, `mtime` 0, level `gzip_level`,
default 3).

Compression changes only the bytes on the wire. The JSON is serialized
exactly as it would be uncompressed and then gzipped, so the decompressed
body is byte-identical: billed tokens, the prompt-cache key and the golden
request snapshots do not change.

It is only used when Harness talks to the public API directly:

- `base_url` is `https://api.anthropic.com` on the default port, with no
  path. A custom `base_url` (a gateway, a local proxy, another port) is
  never compressed.
- No proxy is set (`HTTPS_PROXY`, `ALL_PROXY` and their lowercase forms)
  and no custom CA bundle is set (`SSL_CERT_FILE`, `SSL_CERT_DIR`).

`gzip_requests = off` in `[provider.anthropic]`, or the environment variable
`HARNESS_GZIP_REQUEST_BODIES=0`, turns it off.

The API is observed to accept only gzip, so gzip is the only encoding used.
If a server rejects it anyway, Harness falls back:

- A rejection is any 415, a 400 whose error is
  `request_body_encoding_unsupported` or says the request body is not valid
  JSON, or a 400 or 403 from a hop that is not Anthropic (no `request-id`
  header and no Anthropic error JSON).
- The rejected response is closed first (a streaming body that never ends
  cannot hold the call open). The raw body is then sent once more, and gzip
  stays off for the rest of the process. A warning is logged.
- If the raw resend fails too, that error is returned without further
  fallback.

Every response carries a `harness` opaque block
`{"type": "request_body", "provider": "anthropic", "raw_bytes": ..., "wire_bytes": ..., "encoding": "gzip"}`
(`encoding` is absent when the body went out raw), so the
`model_call_finished` event records both sizes and the request inspector
shows them. Read it with `openai::notice::request_body(&message)`. No
provider sends `harness` blocks back to a model.

`AnthropicProvider::count_tokens` calls the token counting endpoint with
the same body minus the generation-only fields.

The benchmark `upload_benchmark_reports_time_saved_at_simulated_link_speeds`
in `tests/gzip_requests.rs` sends a coding conversation to a local server
that reads at 10, 100 and 1000 Mbit/s and prints a table (run it with
`-- --nocapture`; `HARNESS_GZIP_BENCH_KIB` sets the body size, default 256).
For a 280 KB body, gzip shrinks it to 47 KB. Upload time drops from about
240 ms to 60 ms at 10 Mbit/s and from 34 ms to 27 ms at 100 Mbit/s. At
1000 Mbit/s, compressing takes longer than the upload it saves. These
figures come from a debug build, where compression is much slower than in
a release build.

## Thinking across tool loops

Send the assistant message back unchanged on the next request. The builder
then rebuilds every thinking block exactly:

- Signed reasoning becomes a `thinking` block with the same text and
  signature.
- `redacted_thinking` and other `anthropic` opaque blocks are sent back
  byte-for-byte, in their original order.

Some blocks cannot be checked by the API, so the builder leaves them out:

- reasoning that has no signature
- opaque blocks from other providers

## Effort and thinking budget

The agent's `ThinkingConfig` is mapped to each model's thinking style, which
comes from the model table:

| Model style | Sent |
|---|---|
| Adaptive (current Claude models) | `thinking: {type: adaptive, display: summarized}` plus `output_config.effort`. If only a budget is set, it is converted to an effort level: up to 4,096 tokens is low, up to 16,384 is medium, anything higher is high. |
| Budget (Claude Haiku 4.5) | `thinking: {type: enabled, budget_tokens}`. If only an effort level is set, it is converted to a budget: low 2,048, medium 8,192, high 24,576 tokens. The budget is at least 1,024, and `max_tokens` is raised above it. |

Notes:

- `display: summarized` is what lets reasoning stream live. The API's
  default is to omit it.
- With no `ThinkingConfig`, the `thinking` field is left out and the model
  uses its default. Claude Opus 5.5 and Fable models always think.
- A Claude model that is not in the table gets adaptive thinking and
  conservative limits (32k context window, 4,096 output tokens).
- `temperature` is never sent, because API version 2026-10-21 rejects
  sampling parameters.

## Prompt caching

The request follows the stable-prefix layout: tools, then system, then
messages. Up to four `cache_control: {type: ephemeral}` breakpoints are
placed:

1. on the last tool definition
2. on the system prompt (its last block)
3. on the last cacheable block of the final message
4. on the last cacheable block of the user turn before the final one, so the
   next call reads what this call wrote

When the request has system layers (`ChatRequest::system_layers`: the
memory index and skill listing of a session prefix), the system prompt is
sent as one text block per layer (the same text). Breakpoints then go, in
this order, on the last tool, the last system block, the inner layer
boundaries starting with the end of the base prompt (while one is left for
the history), and the history.

Thinking and opaque blocks never get a marker, because they must be sent
unchanged. `RequestOptions { cache: CacheMode::Off, .. }` turns caching off.

## Usage

Usage is read from `message_start` and updated by `message_delta`:

| API field | `Usage` field |
|---|---|
| `input_tokens` | `input_tokens` (excludes cache) |
| `output_tokens` | `output_tokens` |
| `cache_read_input_tokens` | `cache_read_tokens` |
| `cache_creation_input_tokens` | `cache_write_tokens` |

## Tests

`crates/harness-providers/tests/anthropic_provider.rs` runs the provider
against a local mock server. It covers:

- streaming and parallel tools
- a two-turn tool loop that checks thinking blocks are sent back exactly
- a retry after an overload in the middle of a stream
- error mapping

`tests/gzip_requests.rs` covers request compression against mock servers
that capture the exact wire bytes. It checks byte identity after
decompression, the 4 KiB threshold, `count_tokens`, `auto` and `off`, every
kind of rejection and fallback (including a streaming rejection whose body
never ends), and the benchmark.

The live smoke test is `#[ignore]` and needs `HARNESS_LIVE_ANTHROPIC=1` and
`ANTHROPIC_API_KEY`.
