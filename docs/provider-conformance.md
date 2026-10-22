# Provider conformance suite

`crates/harness-providers/tests/conformance.rs` holds every provider
(Anthropic, OpenAI, OpenRouter, and the fake provider) to one behaviour
contract, so a regression in one provider cannot silently degrade agents.

| Scenario | Contract |
|----------|----------|
| `plain_text` | text `Hello!`, stop `end_turn`, usage reported |
| `single_tool` | one `read` call with `{"path": "src/main.rs"}`, stop `tool_use` |
| `parallel_tools` | two calls, in order, with distinct ids |
| `malformed_tool_json` | the call survives; raw arguments are kept as a string for repair |
| `thinking` | a reasoning carrier (signature or encrypted item) comes back and is resent verbatim |
| `max_tokens` | stop `max_tokens` |
| `rate_limit` | a 429 is retried once, then the call succeeds |
| `overloaded` | a mid-stream overload is retried and the partial output discarded |

## Fixtures

Fixtures live in `tests/fixtures/conformance/<provider>/<scenario>.txt` and
are replayed by a local mock server, offline. A fixture is one or more HTTP
replies served in order:

```text
# comment
--- 429; retry-after-ms: 1
{"type": "error", "error": {"type": "rate_limit_error", "message": "..."}}
--- 200
event: message_start
data: {...}
```

A test scans every fixture for credentials (`sk-...` keys, auth headers), so
an unredacted recording cannot be committed.

## Live smoke tests

`live_anthropic`, `live_openai` and `live_openrouter` run against the real
APIs only when `HARNESS_LIVE_TESTS=1` and the provider's key variable
(`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`) is set;
otherwise they do nothing.

```sh
HARNESS_LIVE_TESTS=1 ANTHROPIC_API_KEY=... cargo test -p harness-providers --test conformance live_
```

With `HARNESS_RECORD_FIXTURES=<dir>` the live calls go through a local
forwarding proxy that writes each response, redacted, in the fixture format
to `<dir>/<provider>_live.txt`, ready to be split into scenario fixtures.
