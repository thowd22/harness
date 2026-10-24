# OpenAI provider

`harness_providers::openai::OpenAiProvider` talks to the OpenAI
**Responses API** (`POST {base_url}/responses`) with streaming. It uses the
shared transport (timeouts, stall detection, retries) described in
[providers.md](providers.md).

```ini
[provider.openai]
api_key_env = OPENAI_API_KEY
; base_url = https://api.openai.com/v1   ; default

[models]
gpt = openai/gpt-5
```

## Behaviour

- **Stateless.** Every call sends `store: false` and the whole
  conversation. Nothing is kept on OpenAI's side between turns.
- **Tools.** Tools are sent as `function` tools with
  `parallel_tool_calls: true`. Each call in a response becomes its own
  tool-use block, keyed by the OpenAI `call_id`. Tool results go back as
  `function_call_output` items. A failed tool result is prefixed with
  `Error: `. Image results are sent as `input_image` parts.
- **Reasoning models** are the o-series (`o1`, `o3`, `o4-mini`, ...),
  `gpt-5*` (except `*-chat*`) and `codex-*`. For these models the request
  includes `reasoning.summary = "auto"` and
  `include: ["reasoning.encrypted_content"]`. `temperature` is left out
  because reasoning models reject it.
  - Reasoning summaries stream live as reasoning deltas, so the agent view
    can show them while the model works.
  - The encrypted reasoning item comes back as an opaque block (provider
    `openai`). It is sent back unchanged on the next turn, so the model
    keeps its chain of thought across tool loops.
  - If a model returns only encrypted reasoning with no summary, the
    response carries a `harness` notice block
    `{"type": "reasoning_hidden", "provider": "openai", "reason": "encrypted_only"}`.
    The UI should then show "reasoning not exposed" instead of nothing.
    Read it with `openai::notice::reasoning_hidden(&response)` or
    `Notice::from_event` while streaming.
- **Effort.** The agent's `effort` (`ThinkingConfig::effort`) maps to
  `reasoning.effort` (`low`, `medium`, `high`). If only a token budget is
  set, it is bucketed: up to 4096 tokens is `low`, up to 16384 is `medium`,
  and anything larger is `high`. With no setting, the model's default
  effort applies.
- **Prompt cache hints.** Every request carries a `prompt_cache_key`, so
  OpenAI routes requests that share a prefix to the same prompt cache. The
  key is `harness-` plus a truncated SHA-256 of the instructions and the
  tool definitions (the reusable prefix), so it stays the same for every
  turn of a session and is shared by subagents with the same instructions
  and tools; it reveals nothing about the prompt. `prompt_cache_retention`
  is not sent by default, so your organisation's own default applies:
  organisations without Zero Data Retention already get `24h` on the
  models that support extended caching, and ZDR organisations keep
  in-memory caching (5-60 minutes). To ask for `24h` explicitly, opt in:

  ```ini
  [provider.openai]
  prompt_cache_retention = 24h   ; default (send nothing) | 24h
  ```

  With `24h`, requests for the models that support extended caching
  (`gpt-5.5`, `gpt-5.5-pro`, `gpt-5.4`, `gpt-5.2`, the `gpt-5.1` family,
  `gpt-5`, `gpt-5-codex`, `gpt-4.1`, and their dated snapshots;
  `openai::request::EXTENDED_RETENTION_MODELS`) carry
  `prompt_cache_retention: "24h"`; other models (`gpt-5-mini`, `gpt-4o`,
  the o-series, GPT-5.6 and later, which use explicit cache breakpoints
  instead) never get it. For a ZDR organisation this overrides the
  in-memory policy, so check your organisation's retention posture first;
  whether OpenAI accepts it from ZDR organisations has not been checked.
  With `auth = chatgpt` the key is sent and the retention is always left
  out, because the plan route rejects it. The cache hit rate has not been
  checked live yet; that waits for an OpenAI API key (TASK-128).
- **Usage.** `input_tokens` excludes cached tokens, `cache_read_tokens` is
  OpenAI's `cached_tokens`, and `reasoning_tokens` is reported as a subset
  of output.
- **Stop reasons.** A completed response with tool calls stops with
  `tool_use`, and one with a refusal stops with `refusal`. An `incomplete`
  response maps its reason (`max_output_tokens` gives `max_tokens`,
  `content_filter` gives `refusal`).
- **Errors.** In-stream `error` and `response.failed` events are typed.
  `rate_limit_exceeded` and `server_error` are retried like the HTTP 429
  and 5xx errors. A stream that ends before `response.completed` is
  retried as a connection error.
- **Not supported.** The Responses API has no stop sequences, so
  `stop_sequences` is ignored. This is logged at debug level.
- **Request bodies are sent uncompressed.** Whether the API accepts gzip
  or zstd request bodies has not been probed live yet. See "Request
  compression on OpenAI and OpenRouter" in providers.md.

## Sign in with ChatGPT

With a ChatGPT Plus or Pro plan you can use OpenAI models on your plan
instead of paying per token. OpenAI allows this for open-source and locally
hosted apps such as harness
([Sign in with ChatGPT](https://developers.openai.com/cookbook/articles/sign-in-with-chatgpt)).

```sh
harness login openai --chatgpt      # or `harness login openai` and choose 4
```

In the TUI, `/login openai` lists *Sign in with ChatGPT* as the fourth
method (Up / Down and Enter, or click it).

```ini
[provider.openai]
auth = chatgpt            ; set by the login; api_key (default) | chatgpt
; chatgpt_headless = true ; allow it in harness run / wf / eval on this machine
```

### What the sign-in does

1. harness listens on `127.0.0.1:1455` (later sign-ins may use another free
   port) and opens the system browser (`$BROWSER`, `xdg-open`, `open` or
   `start`). The URL is also printed, in case no browser opens.
2. The authorization request carries a fresh `state`, `nonce` and PKCE
   `S256` challenge, the per-installation `ext_agent_host_id`
   (`urn:uuid:…`, created once), and the scopes
   `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`
   for the resource `https://api.openai.com/v1`.
3. On the first sign-in harness sends `client_id=dynamic_agent_client` and
   `agent_name_hint=harness`; OpenAI registers the client and returns the
   issued `client_id` on the callback. It is saved before the code is
   exchanged and reused from then on. There is no client secret.
4. The code is exchanged with the PKCE verifier. The ID token's RS256
   signature is checked against the issuer's JWKS, as are `iss`, `aud` (the
   issued client id), `exp` and the `nonce`.
5. The granted scopes must include `chatgpt.tokens.use.direct`. Identity
   alone does not allow plan usage. If plan usage was not granted, the
   sign-in fails with a message saying so, no tokens are stored, and you can
   sign in again and allow it, or use an API key.
6. Declining in the browser, a callback with the wrong `state`, or no
   answer within 5 minutes ends the sign-in without saving anything.

### Where the tokens live

OAuth tokens rotate, so they are not written to the hand-edited
harness.ini. They go to the token store in the data directory:

```text
<data_dir>/auth/              mode 0700
  chatgpt-host.json           ext_agent_host_id (kept across logouts)
  chatgpt-auth.json           issued client_id; access, refresh and ID tokens; expiry; scopes; account (mode 0600)
```

The data directory is a built-in protected path, so agent tools can never
read, copy or send these files (see [data-dir.md](data-dir.md)), and it is
never synced to remote hosts. The tokens are added to the redactor at
startup, and they are sent only in the `Authorization` header (marked
sensitive), so they never appear in events, logs or the screen.

Access tokens last about an hour. harness refreshes one when it is within
5 minutes of expiry, before the request, and saves the rotated refresh
token. Concurrent agents in one process share one refresh, and another
harness process's refresh is picked up from the store. If the refresh token
is expired or revoked (`invalid_grant`, `refresh_token_expired`,
`refresh_token_reused`, ...), the stored tokens are deleted and requests
fail with `run harness login openai to sign in again`. Network and server
errors leave the tokens alone.

`harness logout openai` deletes the tokens (and the `auth` line); the client
registration is kept for the next sign-in.

### Requests on your plan

With `auth = chatgpt`, every OpenAI request goes to the Responses API
(`POST https://api.openai.com/v1/responses`, or `base_url`) with the access
token, `store: false` and streaming. Fields the plan route rejects are left
out: `temperature`, `top_p`, `max_output_tokens`, `truncation`, `metadata`,
`user`, `prompt_cache_retention` and the others in
`chatgpt::UNSUPPORTED_REQUEST_FIELDS`; `prompt_cache_key` is kept. The API key
in the section is not used, and there is never a silent fallback to it.

Plan models are the GPT-5 and Codex families; the model catalog shows them
with the `ChatGPT` auth method and no per-token price (see
[models.md](models.md)).

### Usage limit

When the plan's usage limit for harness is reached
(`subscription_sharing_usage_limit_exceeded`, HTTP 429 or a
`response.failed` event), the call fails at once without retries. The error
is `ProviderError::UsageLimit`, whose action (`ProviderError::action()`) is
**Manage usage**, opening `https://chatgpt.com/settings/usage`. The message
reads `ChatGPT plan usage limit reached: … Manage usage at
https://chatgpt.com/settings/usage; harness does not switch to an API key
on its own`. To keep working, wait, change your plan, or switch to an API
key yourself (`harness login openai`).

The agent loop keeps the action: the turn stops with it, and the `error`
event records it (`action: {label, url}`, see [events.md](events.md)). In
the TUI the turn ends with a **The agent stopped** panel showing the message;
its first button, **Manage usage**, has focus, so Enter opens the usage page
in the browser (clicking it does the same), and Esc or `[ close ]` closes the
panel. Headless runs print the message, which includes the URL.

### Headless runs, workflows and evals

The ChatGPT credential is personal, so `harness run`, `harness wf run` and
`harness eval run` (which runs `harness run`) refuse `auth = chatgpt` with
an error unless `[provider.openai] chatgpt_headless = true`. Even then it is
refused when the `CI` environment variable is set. Give CI an API key
(`auth = api_key`). Only the interactive TUI uses the sign-in without that
setting.

### Testing

`harness_providers::chatgpt::mock` has a local mock authorization server
and Responses endpoint (`MockAuthServer`), a `MockBrowser` that follows the
consent redirect, and a `TestSigner` for RS256 ID tokens. The tests in
`crates/harness-providers/tests/chatgpt_sign_in.rs` cover success, denied
consent, missing plan scope, a forged `state`, refresh, concurrent
refreshes, an expired refresh token and the usage limit, all offline.
