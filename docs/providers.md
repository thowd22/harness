# Providers: message model, transport and fake provider

All model providers (Anthropic, OpenAI, OpenRouter and the fake provider)
speak one normalized model defined in `harness-providers`. The agent, the
workflows, the map view and the evals never see a vendor wire format.

## Normalized model (`harness_providers::model`)

- `ChatRequest`: model id, optional system prompt, messages, tools
  (`ToolSpec` with a JSON Schema), `ToolChoice`, `max_tokens`, temperature,
  `ThinkingConfig` (token budget and/or effort) and stop sequences.
- `Message { role, content: Vec<ContentBlock> }`. The roles are `user` and
  `assistant`. The system prompt lives on the request.
- `ContentBlock` covers `text`, `image` (base64 or URL), `tool_use`,
  `tool_result` (text and image parts, `is_error`), `reasoning` (text plus an
  optional signature that must be sent back unchanged) and `opaque`. An opaque
  block holds raw provider JSON, such as Anthropic `redacted_thinking` or
  OpenAI encrypted reasoning, and is sent back to the provider byte-for-byte.
- `Usage` has five counters: input, output, cache read, cache write and
  reasoning tokens. `input_tokens` excludes cache reads and writes, and
  reasoning tokens are a subset of output.
- `StopReason { kind, raw }` holds the normalized `StopKind` (`end_turn`,
  `tool_use`, `max_tokens`, `stop_sequence`, `pause`, `refusal`,
  `context_limit`, `other`) and also keeps the provider's raw value.
- `Capabilities` lists support for tools, parallel tool calls, thinking,
  caching and images, plus the context window and max output tokens.

## Provider trait (`harness_providers::provider`)

`Provider::stream(request, cancel)` returns a stream of `StreamEvent`s:
`MessageStart`, text, reasoning and tool-argument deltas keyed by block
index, `Opaque`, `BlockStop`, `Retry` and finally `MessageStop { stop, usage }`.
`Provider::complete` and `collect` fold a stream into a `Response`.

A `Retry` event with `discard_partial: true` means that the output streamed
so far belonged to an aborted attempt. Consumers must drop it, and
`ResponseBuilder` does this automatically. Tool arguments that are not valid
JSON are kept as a JSON string. The agent then reports an actionable tool
error instead of failing the whole call.

## Transport (`http`, `sse`, `retry`)

Every provider shares one transport: reqwest over rustls, with HTTP/2 when
the server offers it.

| `HttpConfig` field | Default | Meaning |
|---|---|---|
| `base_url` | per provider | Request paths are appended to it |
| `connect_timeout` | 10s | TCP and TLS connect |
| `request_timeout` | 600s | Whole request, non-streaming calls only |
| `stall_timeout` | 90s | Abort and retry when no bytes arrive (headers or body) for this long |
| `retry.max_retries` | 5 | Retry cap after the first attempt |
| `retry.base_delay` / `max_delay` | 1s / 60s | Exponential backoff bounds |
| `retry.jitter` | 0.5 | Each delay is randomized within `[d*(1-jitter), d]` |
| `retry.max_retry_after` | 300s | A longer server `retry-after` is not waited for and the error is returned |
| `compression` | none | `RequestCompression`: gzip bodies of `min_bytes` (4 KiB) or more for the listed paths. If the server rejects the encoding, the raw body is resent once and the shared `GzipLatch` turns gzip off for the process. Only the Anthropic provider sets it (see providers-anthropic.md) |

These are the following calls:

- **Retried:** 429 (rate limited), 529 or any "overloaded" error, 408, 409,
  5xx, connection failures, timeouts and stalls.
- **Not retried:** 400, 401/403 (`Auth`) and other 4xx.
- **Server wait honoured:** a `retry-after-ms` or `retry-after` header
  (seconds or an HTTP date) replaces the computed backoff.
- **Retries exhausted:** the call fails with `RetriesExhausted { attempts, last }`.
- **Retry events:** each retry produces a `RetryEvent` with the attempt
  number, cap, delay, reason and error message. The UI uses it to show
  "retrying in 4s (429 rate limited, attempt 2/5)".
- **Cancellation:** every call takes a `tokio_util` `CancellationToken`.
  Cancelling ends the call promptly with `Cancelled`, including during a
  backoff sleep.

The SSE parser accepts `\n`, `\r\n` and `\r` line endings. It joins
multi-line `data:` fields, ignores comments and keep-alives, and handles
chunks that split a line ending or a UTF-8 character. If the stream ends
without a final blank line, the last event is still delivered.

### Request compression on OpenAI and OpenRouter

Only the Anthropic provider compresses request bodies. Neither OpenAI nor
OpenRouter documents support for compressed request bodies, so
`harness_providers::probe` checks it instead of guessing. It sends the same
tiny request once raw, as the control, and then once per encoding
(`Content-Encoding: gzip` and `zstd`). Each answer is classified as:

- **accepted:** a 2xx status. A server that ignores the header would fail
  to parse the compressed bytes as JSON, so a 2xx shows it decoded them.
- **rejected:** a 400, 403, 411, 413, 415 or 422 status while the control
  succeeded.
- **inconclusive:** a failed control, 401, 429, 5xx or a network error.

`ProbeReport::decision()` then gives `Enable(encodings)`, `LeaveOff` or
`Inconclusive`. `needs_follow_up()` is true when gzip was accepted.

The probe is opt-in and never runs during normal use. It is an ignored
live test:

```sh
HARNESS_LIVE_TESTS=1 OPENROUTER_API_KEY=... [OPENAI_API_KEY=...] \
  cargo test -p harness-providers --test compression_probe -- --ignored --nocapture
```

- **OpenRouter:** a non-streaming chat completion with `max_tokens: 1` to
  `mistralai/mistral-nemo`. Set `HARNESS_PROBE_OPENROUTER_MODEL` to use a
  different model. OpenRouter has no token-count endpoint.
- **OpenAI:** an input-token count at `POST /v1/responses/input_tokens`,
  which generates no output.

Findings:

| Provider | Probed | gzip | zstd | Decision |
|---|---|---|---|---|
| OpenRouter | 2026-10-22, live | rejected: `400 JSON parsing failed` | rejected: `400 JSON parsing failed` | **leave off** |
| OpenAI | not yet: no API key was available | unknown | unknown | **leave off until probed** |

OpenRouter parses the compressed bytes as JSON and fails, so it ignores
`Content-Encoding` on requests. No follow-up is needed for it. OpenAI could
not be probed because no OpenAI API key was available, and an
unauthenticated request returns 401 before the body is read. Its
compression stays off until someone runs the probe with a key. If OpenAI
accepts gzip, enabling it gets a follow-up task that reuses
`RequestCompression` and `GzipLatch`, with the same fallback design as
Anthropic.

Proposed `harness.ini` keys for `[provider.<name>]` are `connect_timeout`,
`request_timeout`, `stall_timeout` and `max_retries`. They map onto
`HttpConfig` but are not parsed yet; the config crate owns that.

## Fake provider (`harness_providers::fake`)

`FakeProvider` replays a script, one turn per call, and records every request
it receives (`requests()`, `last_request()`). Clones share the script and the
log. When the script runs out, a call fails with
`ProviderError::Other("fake provider script exhausted ...")`.

```yaml
capabilities: { tools: true, context_window: 200000 }   # optional
responses:
  - delay_ms: 50          # wait before the first event
    chunk_delay_ms: 5     # wait between events
    chunk_size: 4         # characters per delta (0 = one delta per block)
    content:
      - reasoning: { text: "Need the file", signature: sig-1 }
      - text: "Reading it now."
      - tool_use: { id: call_1, name: read, input: { path: src/main.rs } }
      - opaque: { provider: anthropic, data: { type: redacted_thinking, data: xyz } }
    stop: tool_use        # default: tool_use if there is a tool call, else end_turn
    usage: { input_tokens: 120, output_tokens: 30, cache_read_tokens: 0 }
  - error: { kind: rate_limited, message: slow down, retry_after_ms: 100 }
  - content: [ { text: "Done." } ]
```

Error kinds are `rate_limited`, `overloaded`, `server` (with `status`),
`auth`, `invalid_request`, `connection`, `timeout`, `stalled` and `other`.
A turn with only `error` fails before streaming. A turn with content and an
`error` streams the content first and then fails. Load a script with
`FakeScript::from_yaml`, `FakeScript::from_yaml_file` or
`FakeProvider::from_yaml`. Delays use tokio time, so tests can run them
instantly with `#[tokio::test(start_paused = true)]`.

## Logging in

`harness login <provider>` adds a key without hand-editing harness.ini:

```sh
harness login anthropic                      # paste the key; input is masked
harness login openai --api-key-env OPENAI_API_KEY
harness login openrouter --api-key-cmd 'pass show openrouter'
pass show anthropic | harness login anthropic # stdin that is not a terminal: one line
harness logout openai                        # asks first; --yes skips the question
```

On a terminal, login asks how harness should get the key: paste it (stored
as `api_key`), a command that prints it (`api_key_cmd`), or an environment
variable (`api_key_env`). The pasted key is read in raw mode and shown only
as `•` characters. It is never echoed, logged, printed in errors or put in an
event.

Before anything is saved, the key (or what the command or variable yields)
is checked with one cheap authenticated request that generates no tokens:

| Provider | Request |
|---|---|
| anthropic | `GET {base_url}/v1/models?limit=1` |
| openai | `GET {base_url}/models` |
| openrouter | `GET {base_url}/key` (its model list is public, so it cannot check a key) |

A rejected key, an unreachable endpoint or a failing `api_key_cmd` ends with a
clear message and leaves harness.ini untouched. A `base_url` already set in
`[provider.<id>]` is used for the check.

The value is written to `[provider.<id>]` of the harness.ini that harness
reads (`$HARNESS_CONFIG`, else `~/.config/harness/harness.ini`; created when
missing). Every other line and comment stays as it was. The other two
credential keys of that section are removed so the new one is the one in
effect, and the file is rewritten atomically with mode `0600` (a symlinked
harness.ini is followed and its target replaced). A file with INI syntax
errors is not edited; fix it with `harness config check` first.

`harness logout <provider>` removes `api_key`, `api_key_cmd`,
`api_key_env` and `auth` from the section after a `[y/N]` confirmation and
keeps `base_url` and other settings. For `openai` it also deletes the
stored ChatGPT tokens. Without a terminal it needs `--yes`.

For OpenAI, the method menu has a fourth choice, *Sign in with ChatGPT*
(also `harness login openai --chatgpt`). It opens the browser instead of
asking for a key, stores the tokens in the protected token store
`<data_dir>/auth/` and sets `auth = chatgpt` in `[provider.openai]`. Saving
an API key later removes `auth`, so the key is in effect again. See
[providers-openai.md](providers-openai.md#sign-in-with-chatgpt).

`harness doctor` shows the auth method in use for each provider, for example
`auth: API key (api_key_env ANTHROPIC_API_KEY)`.

### Supported auth per provider

| Provider | Auth |
|---|---|
| Anthropic | API key only. Claude subscription login (Free, Pro or Max) is not supported: Anthropic's terms do not allow third-party apps to offer Claude.ai login or to route requests through subscription credentials (see [legal and compliance](https://code.claude.com/docs/en/legal-and-compliance)). |
| OpenAI | API key, or Sign in with ChatGPT (`harness login openai --chatgpt`, or method 4 in the menu): uses your ChatGPT plan and sets `auth = chatgpt`. See [providers-openai.md](providers-openai.md#sign-in-with-chatgpt). |
| OpenRouter | API key. |

### In the TUI: `/login`

`/login` (also in the command palette as "Add, check and save an API key
for a provider") opens a modal; `/login openrouter` skips the provider
choice. The modal walks through the same steps as `harness login`:

1. choose the provider (Up / Down or Tab, Enter, or click it),
2. choose how harness gets the key: paste it, a command, or a variable;
   for OpenAI also *Sign in with ChatGPT*, which skips step 3, opens the
   browser and waits for the sign-in (Enter or `[ try again ]` restarts it
   after a failure),
3. type or paste the value. An API key is shown only as `•` characters;
   a command or variable name is shown as typed (the variable is prefilled
   with the provider's usual one, such as `OPENROUTER_API_KEY`),
4. Enter or `[ submit ]` checks the key with the same cheap request and
   saves it. On failure the reason is shown and Enter or `[ try again ]`
   starts over with an empty field.

Esc or `[ back ]` goes back one step, and a click outside the dialog does
the same. If the dialog is closed while the key is checked, the result is
shown as a toast.

The key goes from the modal to the core in memory only. It is added to the
redactor straight away, so later sessions mask it in events and logs.
It is never put in an event, a log line or the screen. After a
successful login the provider's new credential is used by the next
session (`/new`, `/resume`) and by the model picker.

Under the hood, `harness_config::login::LoginFlow` is a render-agnostic
state machine (provider, method, masked value, verifying, done or failed)
driven by `LoginKey`s and `click(index)`. `harness_tui::login::LoginModal`
renders it with theme tokens and registers each option and button with the
interaction layer. On submit the app sends `UiRequest::Login`; the core calls
`login::apply` in the `harness` crate (resolve,
`harness_providers::login::verify_api_key`, then
`harness_config::ini::edit::save_credential`) and answers with
`CoreEvent::LoginResult`. Typed keys live in a zeroizing `MaskedInput` whose
`Debug` output shows only the length.
