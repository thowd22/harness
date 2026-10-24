# Subagents, scheduling and the shared task list

This page covers three `harness-agent` modules: `subagents` (the
`spawn_agent` tool), `scheduler` (concurrency and rate limits) and `todo`
(the run's shared task list, shown in the goal bar).

## spawn_agent

An agent gets the `spawn_agent` tool only when its YAML lists `subagents:`.

```yaml
name: lead
prompt: coder
tools: [read, edit, bash, todo]
subagents: [researcher, reviewer]
```

The tool takes three arguments:

| Argument | Meaning |
|---|---|
| `agent` | One of the listed subagents. The schema makes it an enum of exactly these names, and any other name is refused with the allowed list. |
| `brief` | A self-contained task description. |
| `files` | Optional paths the child should look at first. |

A child does not see the parent's conversation. It starts with:

- its **own** system prompt, from its agent definition (`prompt:` or
  `prompt_text:`), its own model, tools, `max_turns` and `budget`;
- a single user message containing the brief and a "Relevant files" list.

The child runs as a **child node** of the parent in the same run. Its
`node_started` event has `kind: subagent` and `parent_node_id` set to the
parent, so the map hangs it under its parent. When the parent is cancelled
(Ctrl+C, a budget stop, or a cancelled workflow step), its children are
cancelled too and record `node_finished` with status `cancelled`.

When the child ends its turn, the parent gets a tool result like this:

```
Subagent `researcher` (node 3f2a…) finished: end_turn

<the child's final reply>
```

### Structured output

If the child agent declares `output_schema`, the brief asks it to end its
reply with a JSON object that matches the schema. The harness takes either
the last fenced `json` block or a trailing JSON object, then validates it
against the schema. If the object is invalid, the child gets one
correction turn. If it is still invalid after that, the spawn fails with
the validation error. A valid object is appended to the tool result under
`Structured output:`.

### Limits (harness.ini)

```ini
[general]
max_subagent_depth = 2      ; the top agent is depth 0
max_concurrent_agents = 4   ; shared by subagents and workflow steps
```

Agents at the depth limit are not offered `spawn_agent`, and a deeper spawn
is refused. A spawn that cannot get a slot waits. Until it gets one, it is
recorded as a **queued** node (`node_started` with `queued: true`). The
parent's slot is lent to its children while it waits for them, so nested
agents cannot deadlock the pool.

## Scheduler

`harness_agent::scheduler::Scheduler` is shared by subagents and the
workflow executor:

- **Agent slots.** Each running subagent or workflow step holds one of
  `max_concurrent_agents` slots.
- **Per-provider call limits.** `SchedulerConfig::with_provider_limit` caps
  concurrent model calls per provider. Wrap a provider in
  `ScheduledProvider` to apply the cap. harness.ini has no key for this
  yet, so the binary sets it in code.
- **Rate limits slow things down instead of failing.** A 429 from a
  provider pauses new calls to that provider until its `retry-after`
  deadline, or for 5 s when the response has none. If the transport's own
  retries run out on rate limits before any output arrived, the call is
  retried after the pause, up to 8 times. Each wait appears as a
  `retrying in Ns (429 rate limited, attempt k/N)` notice.

### Staggered fan-out (experimental)

With `[experimental] stagger_subagents = true`, children's requests pass
through `harness_agent::stagger::StaggeredProvider`. Siblings that start
together (several `spawn_agent` calls in one reply, parallel workflow
agent steps) and share a cached prefix (provider, model, thinking, system
prompt and tools) do not all write that prefix to the prompt cache: the
first request goes alone, the others wait until its response begins (at
most 30 s, and never holding a scheduler call slot), then go and read the
cache. If the first one fails before its response begins, a waiting one
leads instead. A prefix stays warm for 4 minutes after its last request.
With `[experimental] cache_prewarm = true` as well, the first sibling sends
a `max_tokens: 0` pre-warm of the shared prefix (a `cache-prewarm` node
under it, Anthropic only), releases the others when it returns and sends
its own request with them. Its cost counts against the `[budget]` limits,
and no pre-warm is sent while one is exhausted. Requests and answers are
the same either way.
See docs/performance.md.

## todo: the shared task list

Agents whose `tools:` include `todo` get a small tool:

| Action | Effect |
|---|---|
| `add` | Adds a task. Takes `title`, plus an optional `id` (generated `t1`, `t2`, … when omitted) and `status`. The caller becomes its owner. |
| `update` | Changes a task. Takes `id`, plus optional `title` and `status`. Pass `claim: true` to become its owner. |
| `list` | Lists every task with its status and owning agent. |

Statuses are `pending`, `in_progress`, `blocked`, `done`, `failed` and
`cancelled`.

Every agent in a run shares one board, subagents included, so the user can
see who is working on what. Changes are applied in order under a single
lock and numbered with an increasing `version`, so parallel agents never
lose writes. Each change is:

- broadcast as a `TaskEvent` to live views (`TaskBoard::subscribe`);
- appended to `tasks.jsonl` in the run directory.

`TaskBoard::open` replays a journal. `TaskBoard::continue_from(old, new)`
carries a previous run's tasks into a resumed session's new run.

Workflows can seed the board from their `goal:` and `tasks:` (see
[workflows.md](workflows.md)). A task linked to a step follows that step's
status and owning agent.

### Planning by default

The built-in `coder` has `todo`, and its prompt and the tool description ask
the model to add each step before starting any task with several steps, so
the goal bar shows the plan. Models still sometimes skip it, so harness also
nudges: when an agent that has `todo` has made `[general] todo_nudge_after`
model calls in one turn (default 3) and the run's board is still empty, its
next request carries a one-paragraph reminder to plan with `todo`
(`harness_agent::todo::TodoNudge`, text in `NUDGE_TEXT`).

- It repeats at most every `todo_nudge_after` calls and stops as soon as any
  task exists (from this agent, a subagent or a workflow).
- It is added to the newest tool-result message, which has not been sent
  yet. Earlier messages never change, so prompt caching is unaffected, and
  the reminder is visible in that request's `model_call_started` event.
- Agents without the `todo` tool never get it. `todo_nudge_after = 0` turns
  it off.
