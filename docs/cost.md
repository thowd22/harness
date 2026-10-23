# Cost tracking and budgets

## Accounting

All accounting comes from the run event log. A `model_call_finished` event
records the provider-reported usage, and the `cost` event that follows
records its price. Unknown prices are never counted as $0. They show as `$?`,
or as `$1.2345+?` when only part of a total is known.

`harness_agent::cost::UsageLedger` folds events into `CallRecord`s and groups
them:

- per node, run and session (`UsageLedger::from_runs(data_dir, &meta.runs)`),
- per UTC day (`daily_spend(data_dir, day)` reads every run of that day),
- per model and per agent.

Each record also gives its context size and prompt-cache hit rate. The
event's usage splits input into `input` (uncached, including tokens
written to the cache), `cached_input` (read from the cache) and
`cache_write` (the part of `input` written to the cache, left out of the
log when zero). Prices charge each at its own rate: `[pricing]` takes
`input,output[,cache_read[,cache_write]]`, catalog prices carry the
provider's cache rates, and missing cache rates default to 0.1x and 1.25x
the input price. OpenRouter's reported cost is used as is (see
[providers-openrouter.md](providers-openrouter.md#behaviour)).

## /cost

`/cost [model|agent|node]` prints `CostReport::lines`: a total line, then
breakdowns by model, by agent and by node, most expensive first. Each row
shows cost, tokens, calls and cache hit rate, plus the tokens written to
the cache when there were any (`· 2600 written to cache`). The status
line shows the same next to the hit rate (`cache 92% wrote 2.6k`), and a
turn's header in node detail shows `(cached 11.9k, written 2.6k)`.

```text
total: $2.1000 · 4200 tokens · 12 calls · cache 81%
by model:
  anthropic/claude-opus-5-5             $2.0000       3200 tok    8 calls · cache 85%
```

## Budgets

Limits come from `[budget]` (`session_usd`, `run_usd`, `daily_usd`,
`on_exceed`) and from YAML (`budget: { usd: ... }` on agents and workflows):

| Scope | Source | Counts |
|---|---|---|
| agent | agent YAML `budget.usd` | one agent loop (node) |
| workflow | workflow YAML `budget.usd` | one workflow run |
| run | `[budget] run_usd` | one run |
| session | `[budget] session_usd` | every run of the session, subagents included |
| daily | `[budget] daily_usd` | every session that day (UTC) |

Every loop of a session shares one `BudgetMeter`. Each loop also has a
`BudgetGuard` request hook, which does the following before every request:

- At 80% of a limit, it records a one-time warning event, for example
  `budget warning: session spend $8.00 is 80% of the $10.00 budget`.
- At the limit, with `on_exceed = stop`, the loop ends with the `budget`
  stop reason (exit code 4 for headless runs).
- At the limit, with `on_exceed = ask`, it asks the user (`BudgetApprover`).
  If the user approves, that limit rises by its original amount and the run
  continues. If the user refuses, the run stops. Headless runs use
  `DenyOverruns`.

Seed the meter with spend that already happened, so limits hold across
resumes and sessions: `with_spent(BudgetScope::Day, daily_spend(...)?.cost_usd)`.
