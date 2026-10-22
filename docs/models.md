# Models, aliases and pricing

You choose the model for each agent yourself. Harness has no hidden router.
Every model is named by a **reference** of the form `provider/model`:

```
anthropic/claude-opus-5-5
openai/gpt-5
openrouter/google/gemini-2.5-pro        # OpenRouter ids are vendor/model
```

The provider is the text before the first `/`, and the model id is
everything after it.

## Aliases (`[models]`)

Aliases are short names for references. You can use them anywhere a model
is expected: `default_model`, agent YAML and `/model`.

```ini
[models]
opus   = anthropic/claude-opus-5-5
fast   = openai/gpt-5-mini
sonnet = openrouter/anthropic/claude-sonnet-5.5   ; replaces the built-in alias
```

These aliases work without any configuration. An alias with the same name in
`[models]` replaces the built-in one.

| Alias | Reference |
|---|---|
| `fable` | `anthropic/claude-fable-5-1` |
| `opus` | `anthropic/claude-opus-5-5` |
| `sonnet` | `anthropic/claude-sonnet-5-5` |
| `haiku` | `anthropic/claude-haiku-5-5` |

A name is resolved in this order:

1. An alias.
2. A `provider/model` reference.
3. A bare model id such as `gpt-5`, if exactly one provider in the built-in
   table has it.

Any other name is an error. The error message lists the known aliases.

## Built-in model table

Harness includes a table of facts for current Claude, OpenAI and common
OpenRouter models:

- context window and maximum output tokens
- whether the model supports tools, parallel tool calls, thinking, prompt
  caching and images
- how reasoning is requested: adaptive thinking with an effort level, a
  thinking token budget, or OpenAI-style `reasoning_effort`
- list prices in USD per million tokens

The prices are a snapshot from 2026-10, so check them against your provider's
price list. OpenRouter references to Anthropic or OpenAI models, such as
`openrouter/anthropic/claude-sonnet-5.5`, reuse the vendor's entry. Dots in
the model id are treated as dashes.

## Price overrides (`[pricing]`)

```ini
[pricing]
; input,output[,cache_read[,cache_write]]  USD per million tokens
anthropic/claude-sonnet-5-5 = 3,15,0.3,3.75
fast = 0.25,2                               ; alias keys work too
```

An override replaces the whole built-in price for that model. If it leaves
out the cache rates, Harness uses these defaults:

- cache reads cost 0.1 × the input rate
- cache writes cost 1.25 × the input rate

Overrides also work for models that are not in the built-in table.

Each call's cost is computed from its usage:

```
input × input_rate + output × output_rate
  + cache_read × cache_read_rate + cache_write × cache_write_rate
```

Reasoning tokens are already counted in output tokens, so they are not
charged twice.

## Unknown models

A reference that is not in the built-in table still works. Harness uses
conservative defaults for it:

- 32k context window and 4,096 output tokens
- tools only: no parallel calls, images, caching or thinking

Harness shows a warning that the model is unknown. If there is no
`[pricing]` entry for the model, it shows a second warning: the cost is
unknown. Unknown cost is reported as unknown, never as $0. Add a `[pricing]`
line to fix that.

## Model catalog (`harness models`, `/model`)

The model catalog is the list of every model you can pick. It merges:

1. the built-in model table above,
2. aliases (built-in and `[models]`). An alias target is always listed, even
   when no other source knows the model,
3. the live model lists of Anthropic (`GET /v1/models`), OpenAI
   (`GET /v1/models`) and OpenRouter (`GET /models`), fetched with the API key
   and `base_url` of each `[provider.*]` section.

Each entry has the model id, provider, aliases, context window, input and
output price, capabilities (tools, reasoning, vision) and auth method.
For Anthropic and OpenAI, facts and prices from the built-in table take
precedence over the live list. The live list fills in models the table does
not know: the Anthropic list gives their context window, output limit and
capabilities, and the OpenAI list gives only ids. For OpenRouter, the live
list's limits and prices take precedence. A `[pricing]` override always
wins. OpenAI's live list leaves out models that cannot chat, such as
embeddings, speech, image, moderation and realtime models.

### Which models are usable

- A model is **usable** only when its provider has working credentials: an
  API key that resolves (`api_key`, `api_key_cmd` or `api_key_env`) and that
  the provider has not rejected. A 401 or 403 from the model list marks the
  key as not working. Other models are listed with the reason and how to fix
  it, for example `no openai API key: set api_key, api_key_cmd or
  api_key_env in [provider.openai]`.
- A model **without tool calling** (some OpenRouter models, OpenAI search
  previews and `chatgpt-*` snapshots) can chat but is flagged as not usable
  for agents, because agents need tools.
- **Sign in with ChatGPT** (OpenAI): entries show the auth method. Plan
  models (the GPT-5 and Codex families) used through ChatGPT have the
  `ChatGPT` auth method and no per-token price, because usage counts against
  your plan. Other OpenAI models need an API key. With only ChatGPT signed
  in, they are listed as `not included in Sign in with ChatGPT`. With both
  an API key and ChatGPT, a plan model is listed once per auth method.

### Caching and refresh

Live lists are cached in `<data_dir>/model-lists/`, one JSON file per
provider. The catalog is built from the cache straight away, so it opens
instantly and works offline. A list is refetched in the background when it
is older than `[general] model_list_ttl` (default `24h`) or came from a
different `base_url`. If a refresh fails, for example offline, the cached
list stays in use and the error is kept for display.

```ini
[general]
model_list_ttl = 6h
```

### `harness models`

```
harness models [--all] [--json] [--refresh | --offline]
```

Prints the usable models in an aligned table grouped by provider: model,
provider, alias, context, input and output price (USD per million tokens,
`plan` for ChatGPT plan models), capabilities, auth method and status. Stale
lists are refreshed first. `--refresh` refetches every list, and
`--offline` uses only the cache. `--all` also lists unusable models with
the reason, and `--json` prints the entries as JSON.

### The model picker (`/model`)

In the TUI, `/model` without an argument (or the palette's "Switch model")
opens a large dialog with every model in the catalog, grouped by provider:

```
╭ Switch model ───────────────────────────────────────────────────────────────╮
│ ›   9 of 9 models                                  ⟳ refreshing model lists │
│    Model              Provider   Alias   Context  In $/M  Out $/M  ...      │
│ anthropic · 3 models                                                        │
│    claude-opus-5-5    anthropic  opus         1M   $5.00   $25.00  ...      │
│ ▸● claude-sonnet-5-5  anthropic  sonnet     200k   $3.00   $15.00  ...      │
│ openai · 2 models                                                           │
│    gpt-5.5            openai     gpt        400k   $1.25   $10.00  ... no … │
│ ↑↓ PgUp PgDn Home End move · Enter switch · Esc close · type to filter      │
╰─────────────────────────────────────────────────────────────────────────────╯
```

- Columns: model, provider, alias, context window, input and output price
  (USD per million tokens, `plan` for ChatGPT plan models, `free`, `?` when
  unknown), capabilities, auth method and status.
- `●` marks the session's model. Models that cannot be used are dimmed and
  the status column says why; choosing one keeps the dialog open and shows
  the reason with the fix. Models without tool calling say `chat only`.
- Type to filter with fuzzy matching on the model id, provider, name and
  aliases. Up, Down and Tab, PgUp and PgDn, Home and End move the
  selection; Enter switches; Esc or a click outside closes. Click a row to
  switch to it, and use the wheel to scroll.
- The dialog is 90 % of the terminal height, centered. It is as wide as its
  widest row; on a narrower terminal it stops 2 cells short of each edge
  and cuts the widest cells (status first, the model id last) with `…`, so
  a row never wraps.
- It opens instantly from the cached live lists. Stale lists are refreshed
  in the background; the top line says `⟳ refreshing model lists` meanwhile,
  and the rows update in place when the refresh ends. A provider whose
  refresh failed shows the error in its group header.

`/model NAME` (an alias or `provider/model`) still switches directly without
the dialog.

## For developers

| What | Where |
|---|---|
| The `/model` picker (layout, keys, mouse) | `harness_tui::model_picker::ModelPicker`; opened by `CoreEvent::ModelPicker`, updated by `CoreEvent::ModelCatalog`, answered with `UiRequest::SetModel` |
| Model references (parsed, `Display`, serde as a string) | `harness_providers::registry::ModelRef` |
| Built-in table plus user aliases and overrides | `Registry::builtin()`, `.with_aliases()`, `.with_price_overrides()` |
| Resolving a name | `Registry::resolve(name) -> Resolved`, which gives `reference`, `info` (`capabilities`, `thinking`), `price`, `price_source`, `warnings` and `cost(&Usage)` |
| Capability lookups for providers | `Registry::shared().capabilities(provider, model)` |
| Prices and cost | `harness_providers::pricing::Price` and `pricing::cost(Option<&Price>, &Usage)` |
| Model catalog (entries, usability, auth method) | `harness_providers::catalog::{Catalog, CatalogEntry, Availability, AuthMethod, Credentials}`. `Catalog::build(&registry, &credentials, &live_lists)` merges the sources. `entry.usable()`, `entry.usable_for_agents()` and `entry.reason()` report usability |
| Cached, background-refreshed catalog | `catalog::CatalogService`: `catalog()` returns the current catalog instantly, `subscribe()` returns a `watch::Receiver` that sees every update, `spawn_refresh(force)` refreshes in the background, `Catalog::refreshing()` drives a refresh indicator, and `set_credentials` applies a login or logout. The binary builds it with `wiring::model_catalog(&cfg, &registry)` |
| Live list parsing and fetching | `catalog::live::{ModelListSource, LiveModel, parse}`, cache in `catalog::cache::{ModelListCache, LiveList}` |
