# OpenRouter provider

`harness_providers::openrouter::OpenRouterProvider` talks to OpenRouter's
OpenAI-compatible **Chat Completions API**
(`POST {base_url}/chat/completions`) with streaming, plus OpenRouter's own
extensions for routing, reasoning and cost.

```ini
[provider.openrouter]
api_key_env = OPENROUTER_API_KEY
; base_url = https://openrouter.ai/api/v1   ; default
app_name = harness
provider_order = anthropic,openai   ; upstream providers to try, in order
allow_fallbacks = true              ; may OpenRouter use others?
; cache_ttl = 5m                    ; Anthropic prompt-cache lifetime: 5m (default) | 1h
; session_id = true                 ; sticky upstream routing per session (default on)
; reasoning_exclude = false         ; leave reasoning out of responses (opt-in)
; service_tier = fast               ; priority tier / Anthropic fast mode, 2x price (opt-in)

[models]
router = openrouter/anthropic/claude-sonnet-4
```

The binary maps the section onto `OpenRouterOptions { app_name,
provider_order, allow_fallbacks, cache_ttl, session_id, reasoning_exclude,
service_tier }`.

## Behaviour

- **Routing.** `provider_order` and `allow_fallbacks` are sent as
  `"provider": {"order": [...], "allow_fallbacks": bool}`. The object is
  left out when neither key is set.
- **Service tier and fast mode (opt-in).** `service_tier = fast` sends
  `"service_tier": "fast"`: OpenRouter serves the request from the model's
  priority endpoint where it has one. For Claude Opus 5.5 that is
  Anthropic fast mode (endpoint `anthropic/fast`, $8/$40 per million
  input/output tokens instead of $4/$20); without a priority endpoint, or
  when it is unavailable, the request falls back to standard endpoints at
  the standard price. `service_tier = default` sends `"default"`: standard
  endpoints only. Unset (the default) sends nothing. Measured on the
  bundled bench (TASK-161.7, see [performance.md](performance.md#fast-mode-nitro-reasoning-and-effort-task-1617)):
  turn time 15.3 s to 7.0 s p50, output 171 to 441 tok/s p50, cost per
  session +75%, eval pass rate unchanged.
- **Routing variants.** A model id may end in OpenRouter's `:nitro`
  (sort endpoints by throughput; priority endpoints are admitted, so for
  Opus 5.5 it picks fast mode at its price when that is the fastest) or
  `:floor` (sort by price). The catalog check and capabilities use the
  base model's entry, e.g. `opus-nitro = openrouter/anthropic/claude-opus-5.5:nitro`.
  Other suffixes must be listed in the catalog themselves.
- **Sticky routing.** Every request carries `"session_id"`: one id per
  harness process (`harness-<run id>`), shared by the main agent,
  subagents, compaction and session summaries, so OpenRouter keeps routing
  the session to the same upstream (and its warm prompt cache) from the
  first request on, also after a pause longer than OpenRouter's default
  stickiness window. `session_id = false` sends none. OpenRouter ignores it
  when `provider_order` pins upstreams. Measured cost: none (TASK-161).
- **Attribution.** `app_name` is sent as the `X-Title` header. If it is an
  `http(s)://` URL, it is also sent as `HTTP-Referer`.
- **Tools.** Tools are sent as `function` tools. Streamed `tool_calls` become
  tool-use blocks. Tool results go back as `tool` messages. Images in tool
  results follow in a user message, because the `tool` role carries only
  text.
- **Reasoning.** The agent's effort is sent as `reasoning.effort`, a token
  budget as `reasoning.max_tokens`.
  - `delta.reasoning` text streams live as reasoning deltas. If a model only
    sends `reasoning_details`, its text and summary parts are streamed
    instead.
  - `reasoning_details`, including signatures and encrypted blobs, are
    merged by index into one opaque block (provider `openrouter`). They are
    sent back on the assistant message of the next turn.
  - Missing reasoning is flagged. If reasoning was requested, or reasoning
    tokens were billed, but none was streamed, the response carries a
    `harness` notice
    `{"type": "reasoning_hidden", "provider": "openrouter", "reason": "not_provided" | "encrypted_only"}`.
  - `reasoning_exclude = true` (off by default) adds `"exclude": true` to
    the `reasoning` object: the model still reasons at the same effort and
    is billed the same, but no reasoning text or details come back, so
    nothing streams into the TUI's thinking view and no signatures are
    sent back on the next turn. It is the OpenRouter counterpart of the
    Anthropic provider's `thinking_display = omitted`. Measured in the
    TASK-161.7 live A/B (effort medium, 2 sessions each): TTFT p50 1.60 s
    shown vs 1.39 s excluded, turn p50 14.9 vs 13.9 s, cost the same;
    within the run-to-run noise, so it stays off by default.
  - There is no cache pre-warm (`[experimental] cache_prewarm`) on
    OpenRouter: a live check on 2026-10-23 (Opus 5.5) showed that it
    ignores `max_tokens: 0` and generates a full answer (22 output
    tokens), so a pre-warm would cost an answer each time.
- **Prompt caching.** Claude models on OpenRouter cache only with explicit
  `cache_control` markers; other models (OpenAI, Gemini, DeepSeek, ...)
  cache automatically and get no markers. For model ids starting with
  `anthropic/` (or `~anthropic/`), the body carries up to four breakpoints,
  placed like the Anthropic provider's: the last tool, the system prompt
  (sent as a content array, one text part per prompt layer: the end of the
  frozen prefix, then layer boundaries), the final message and the user
  turn before the last user turn. User and tool text is then always sent as
  content parts (`[{"type": "text", ...}]`), so a message's bytes stay the
  same when its marker moves on and every call reads what the previous one
  wrote. The markers work on every Anthropic upstream OpenRouter routes to
  (Anthropic, Bedrock, Vertex). `cache_ttl = 1h` adds `"ttl": "1h"` to every
  marker; one-hour writes cost more than five-minute writes (2x input
  instead of 1.25x), so use it only when turns are often more than five
  minutes apart. Prompts below the model's minimum cacheable length (512
  to 4096 tokens depending on the model) are not cached. A live check on
  2026-10-23 (`anthropic/claude-haiku-5.5`) wrote 2,571 tokens on the first
  call and read all of them on the second.
- **Usage and cost.** `usage: {"include": true}` is always sent. Token
  counts are normalized: cache reads and writes are split out of
  `prompt_tokens`, and reasoning tokens are reported. When OpenRouter
  reports `usage.cost`, the response carries a `harness` notice
  `{"type": "cost", "provider": "openrouter", "usd": ...}`. Read it with
  `openrouter::notice::reported_cost(&response)`. Cost accounting should
  prefer this exact figure over a price-table estimate. OpenRouter reports
  cache reads as `prompt_tokens_details.cached_tokens` and writes as
  `prompt_tokens_details.cache_write_tokens`; its `usage.cost` already
  charges both at the upstream's cache rates. When it is missing, the
  price-table estimate uses the catalog's `input_cache_read` and
  `input_cache_write` prices (five-minute writes).
- **Tool support check.** `check_agent_model(model, needs_tools)` loads the
  model catalog. The binary first reads the cached model list in
  `<data_dir>/model-lists/openrouter.json` (the same file `/model` and
  `harness models` use, fresh for `[general] model_list_ttl`, default 24h,
  and only when it came from the same `base_url`): a session start with a
  fresh list makes no catalog request (the download is ~800 KB, 0.3-0.8 s).
  A stale list is used and refreshed in the background. When the list
  lacks the model, or says it cannot call tools, the live catalog
  (`GET /models`) decides; offline, the cached answer stands. Without a
  cached list the catalog is downloaded first and written to the cache.
  It fails when
  the model is unknown, or when the agent uses tools and the model does not
  list `tools` among its supported parameters, with the message:
  "OpenRouter model `x` does not support tool calling, so it cannot run an
  agent that uses tools. ...". Once the catalog is loaded, `stream()`
  refuses tool requests to such models without making a call, and
  `capabilities()` comes from the catalog.
- **Errors.** Mid-stream error chunks (`{"error": {"code": 502, ...}}` or
  `finish_reason: "error"`) are mapped by their HTTP-like code and retried
  when they are transient. A stream that ends without a `finish_reason` is
  retried as a connection error.
- **Request bodies are never compressed.** A live probe on 2026-10-22
  found that OpenRouter rejects gzip and zstd request bodies with
  `400 JSON parsing failed`. See "Request compression on OpenAI and
  OpenRouter" in providers.md.

## Notices

Both OpenAI-style providers attach these facts as opaque blocks with
provider `harness` (`openai::notice`, re-exported as `openrouter::notice`).
No provider ever sends a `harness` block back to a model.
