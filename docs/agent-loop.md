# Agent loop

The agent loop (`harness_agent::agent_loop`) turns model output into tool
calls and back. It is a small, explicit state machine, and everything it does
is recorded as an event in the run log (`events.jsonl`, see `events.md`), so a
run can be visualized, replayed, resumed and evaluated.

```text
 Request ──► Streaming ──► Dispatch ──► Submit ──► Request ...
    │            │            │                       │
    └────────────┴────────────┴──► Done(stop reason) ◄┘
```

| State | What happens | Events |
|-------|--------------|--------|
| Request | check cancel / `max_turns` / budget, make sure every tool call in the history has a result, send the request | `node_started` (first time), `model_call_started` |
| Streaming | forward deltas, fold the stream into a response, price it | `delta`, `model_call_finished`, `cost`, `error` (retry notices) |
| Dispatch | prepare and run the tool calls | `tool_call`, `tool_result`, `file_changed`, `approval_*`, `error` (repairs, loop detection) |
| Submit | append the tool results to the conversation | — |
| Done | run verification, record the outcome | `verify_started`, `verify_finished`, `node_finished` |

## Why a run stops

Each stop reason has its own status in `node_finished`. The `error` field
starts with a stable code, so tools can tell the reasons apart without
parsing prose:

| Reason | `node_finished.status` | `error` prefix |
|--------|------------------------|----------------|
| The model ended its turn | `succeeded` | — |
| `max_turns` reached | `failed` | `max_turns:` |
| Token or cost budget exhausted | `failed` | `budget:` |
| Cancelled by the user | `cancelled` | `cancelled:` |
| Unrecoverable error (provider failure, context window overflow) | `failed` | `error:` |

A `pause_turn` stop sends the request again. `max_tokens` without tool calls
ends the turn normally.

## Cancellation

The cancellation token is passed to the provider stream and raced against the
stream and every tool call. Cancelling drops the running tool's future, which
kills a bash command's whole process group. The conversation is always left
valid: interrupted tool calls get an error result that tells the model the
call may not have taken effect.

## What the model sees

The loop keeps the conversation exactly as it was recorded: messages are
redacted with the run's redactor, and tool results are labeled with
provenance markers (`<tool_result ... trust=...>`). Raw secrets never reach
the model, and a session rebuilt from the log is byte-for-byte the
conversation that was sent (see `sessions.md`).

Each `model_call_started` request payload holds `messages_from` plus only the
messages added since the previous call, and `system` only when it changed, so
the log does not grow quadratically. The first call of every run logs the
whole conversation.

## Tool-call robustness

Problems at the tool boundary never crash the loop. They become short error
results that say how to fix the call:

- **JSON repair.** Arguments that are not valid JSON are repaired when the
  fix is unambiguous: code fences and prose around the JSON, trailing commas,
  single quotes, raw newlines in strings, Python `True`/`False`/`None`,
  truncated strings, unclosed brackets. Every repair is logged (`harness::tools`
  target) and recorded as a recoverable `error` event.
- **Schema validation.** Arguments are checked against the tool's JSON schema
  before the tool runs. Safe mismatches are coerced and logged (`"30"` for an
  integer, `null` for an optional field). Missing fields, unknown fields
  (with a "did you mean" hint) and wrong types are reported together:

  ```text
  Error in `bash`: invalid arguments: `command` is required but missing.
  Fix: Call `bash` again with arguments {"command": string, "timeout"?: integer}.
  ```

- **Unknown tools** get the list of available tools and the closest match.
- **Duplicate call ids** in one turn are renamed (`<id>__2`) and run normally.
- **Missing results.** Any tool call without a result (crash, cancel) gets
  a synthesized error result before the next request.
- **Loop detection.** When the same call (same tool, same arguments) fails
  three times, a `loop detected` event is recorded and a nudge is appended to
  the result telling the model to change approach.

## Execution order

All tool calls of one model turn go to the policy gate together, so they
produce at most one approval prompt. Consecutive read-only calls (`read`) run
in parallel; mutating calls (`write`, `edit`, `bash`) run one at a time, in the
order the model gave them. Results go back in call order.

## Costs and budgets

Each call is priced with the model's price (`[pricing]` overrides the
built-in table). A cost reported by the provider (OpenRouter) wins over the
estimate. Unknown prices are reported as unknown, never as `$0`.
`LoopLimits` caps turns, tokens and dollars per run; `harness run` fills them
from `--max-turns`, `--budget-tokens`, `--budget-usd`, the agent's
`max_turns`/`budget` and `[budget] run_usd`.
