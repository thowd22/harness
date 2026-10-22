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

[models]
router = openrouter/anthropic/claude-sonnet-4
```

The binary maps the section onto `OpenRouterOptions { app_name,
provider_order, allow_fallbacks }`.

## Behaviour

- **Routing.** `provider_order` and `allow_fallbacks` are sent as
  `"provider": {"order": [...], "allow_fallbacks": bool}`. The object is
  left out when neither key is set.
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
- **Usage and cost.** `usage: {"include": true}` is always sent. Token
  counts are normalized: cache reads and writes are split out of
  `prompt_tokens`, and reasoning tokens are reported. When OpenRouter
  reports `usage.cost`, the response carries a `harness` notice
  `{"type": "cost", "provider": "openrouter", "usd": ...}`. Read it with
  `openrouter::notice::reported_cost(&response)`. Cost accounting should
  prefer this exact figure over a price-table estimate.
- **Tool support check.** `check_agent_model(model, needs_tools)` loads the
  model catalog (`GET /models`, cached after the first fetch). It fails when
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
