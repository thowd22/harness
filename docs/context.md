# Context management

harness keeps long sessions coherent and cheap without the model ever
having to think about context size. Everything here lives in
`harness_agent::context` and `harness_agent::compaction`, and runs as a
request hook of the agent loop (`ContextManager`).

## Request layout (prompt caching)

Every request is laid out so that what does not change comes first:

1. the rendered system prompt,
2. the tool definitions (fixed for the session),
3. the memory index (`MEMORY.md` plus the project's notes),
4. the skill listing,
5. the history.

The system prompt of every built-in agent starts with the built-in identity
layer (see [defaults.md](defaults.md)), so it is part of the most stable
bytes of the request and never moves the cache boundary.

Layers 1, 3 and 4 are frozen once at session start (`SessionPrefix::freeze`)
and never recomputed mid-session. Memories written and skills added during a
session take effect in the next one. Because the prefix bytes never change,
the provider's prompt cache stays valid on every turn. The layer boundaries
travel with every request (`ChatRequest::system_layers`, set by
`Conversation::from_prefix`). Anthropic gets one system block per layer and
cache breakpoints at the end of the tools, at the end of the frozen system
text, at the end of the base system prompt (the layer most stable across
sessions) and in the history, within its limit of four. Tests (`tests/request_layout.rs`) build the real Anthropic,
OpenAI and OpenRouter request bodies for a multi-turn tool loop and check
that the prefix is byte-identical from turn to turn.

The cache hit rate of each call is derived from the usage recorded in
`model_call_finished` (`cached_input / (input + cached_input)`) and shown by
`/cost`.

## Reasoning across turns

Reasoning is kept exactly as the provider returned it: Anthropic's signed
thinking blocks, OpenAI's encrypted reasoning items and OpenRouter's
`reasoning_details` are all sent back unchanged. Clearing only happens when
`ContextSettings::reasoning` is set to `ReasoningRetention::DropCompletedTurns`.
That setting drops the reasoning of turns that finished before the latest user
prompt and never touches the current tool loop.

## Token accounting and the context tracker

Each model call's provider-reported usage is in the event log.
`harness_agent::cost::UsageLedger` groups it per call, node, run, session
(the session's runs) and UTC day. `context_by_node()` gives each node's
latest and peak context size.

Before sending a request, `ContextTracker` estimates its size. It takes the
previous call's reported input plus output as an anchor and adds an estimate
(4 bytes per token) of the messages appended since. This number drives
compaction.

## Tool-result elision

Tool results larger than `[context] elide_tool_results_over` tokens are
replaced with a short stub once they are `keep_recent_turns` assistant turns
old:

```text
[harness: earlier output elided] This `bash` result (about 12000 tokens) is
stored by the harness. To see it again, call the read tool with path
"spill:<id>".
```

The full text goes to the run's blob store, and the read tool's
`spill:<id>` path brings it back. Elision happens in batches. Nothing changes
until the oldest eligible result is twice that age, and then every eligible
result is elided at once. The cached prefix therefore changes at most once
every `keep_recent_turns` turns, not on every turn.

## Compaction to a structured handoff

When the next request is estimated at `[context] compact_at` of the model's
window, or when the user types `/compact [focus...]`, the older history is
summarized into a handoff with these sections:

- Goal
- Decisions
- Current state
- Files touched (the harness lists the paths of `write`/`edit`/`read` calls
  first, then adds the model's own entries)
- Next steps
- Open questions

The most recent `keep_recent_turns` assistant turns, and the prompt before
them, stay verbatim. The handoff becomes the first user message.

Each compaction is recorded as a child node named `compaction` under the
agent node. The node holds its own `model_call_*` and `cost` events, so it
appears in the agent map and node detail and its cost is counted. The
rewritten history is re-recorded with the next request, so resuming a session
rebuilds the compacted conversation. If compaction fails, the problem is
logged and the run continues.

## No context anxiety

Models tend to wrap up early when they believe the context is nearly full.
To avoid that:

- No request carries token counts, percentages or remaining-context notices.
  Stubs and handoffs contain no numbers about the context, and tests check
  every request with `mentions_remaining_context`.
- `CONTEXT_PROMPT_SECTION` is text for default prompts that tells the model
  the harness handles context length and that it should keep going.

## Initial context budget (CI)

`tests/initial_context.rs` measures the built-in `coder` agent's initial
context: rendered system prompt, tool definitions (read, write, edit, bash,
todo, memory), the memory index of a fresh drive and the skill listing. It
fails above 4000 tokens (`DEFAULT_INITIAL_BUDGET`). The test always prints a
line like this to the CI log:

```text
harness CI: default agent `coder` with tools [...]: initial context 1308 tokens (threshold 4000): system 475, tools 818, memory index 15, skills 0
```

## Settings

`ContextSettings::from_config(&cfg.context, context_window)` reads `[context]`:

| Key | Default | Meaning |
|---|---|---|
| `compact_at` | 0.8 | Compact when the next request reaches this fraction of the window |
| `keep_recent_turns` | 6 | Assistant turns kept verbatim, and the age at which results may be elided |
| `elide_tool_results_over` | 4000 | Tool results above this many tokens can be elided |
