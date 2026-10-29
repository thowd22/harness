# Slash commands

Every slash command is in one registry (`harness_agent::commands::CommandRegistry`):
built-ins, workflows, skills and agent switches. Input completion and the
command palette both read from it.

## Built-ins

| Command | Arguments | What it does |
|---|---|---|
| `/help` | `[command]` | Show commands and keys, or the details of one command |
| `/new` | | Start a new session in this project |
| `/resume` | `[session...]` (a name, `last`, an id prefix or a `#tag`) | Resume a saved session; with no argument, pick one in a searchable dialog ([sessions](sessions.md)) |
| `/rename` | `[name...]` | Rename this session (without a name: show it) |
| `/model` | `[model]` (alias or provider/model) | Switch the model directly, or open the model picker without an argument |
| `/effort` | `[default\|minimal\|low\|medium\|high\|xhigh\|max\|ultra]` | Set the reasoning effort of the session's agent; without an argument, open a picker showing the active effort ([below](#effort)) |
| `/mode` | `[ask\|auto-edit\|auto]` | Show or switch the approval mode |
| `/agents` | | List agents and switch to one |
| `/workflows` | | List workflows and their arguments |
| `/skills` | | List skills and their arguments |
| `/memory` | `[show\|search\|dream] [query...]` | Browse, search or consolidate memory |
| `/runs` | `[run]` | List runs or open one in the agent map |
| `/remote` | `[list\|test] [host]` | List remote hosts or test a connection |
| `/cost` | `[model\|agent\|node]` | Spend and tokens by model, agent and node |
| `/compact` | `[focus...]` | Condense the conversation into a handoff now |
| `/undo` | | Restore files from before the last agent turn |
| `/diff` | `[path]` | Show the changes made in this session |
| `/context` | | Show what the next request carries, by layer |
| `/config` | `[show\|check]` | Show the effective config or check harness.ini |
| `/doctor` | | Check config, providers, sandbox and remotes |
| `/jobs` | | Background bash jobs with state, elapsed time and command; open a job's output or stop it ([tools](tools.md#background-jobs-run_in_background)) |
| `/mcp` | | MCP servers with status, transport, tools and auth; restart, disable for the session or view logs ([MCP](mcp.md#in-the-tui)) |
| `/login` | `[anthropic\|openai\|openrouter\|mcp] [server]` | Add, check and save an API key in a modal, or Sign in with ChatGPT for openai ([Logging in](providers.md#in-the-tui-login)); `mcp [server]` authorizes remote MCP servers with OAuth ([MCP](mcp.md#authorization-oauth)) |
| `/quit` | | Quit harness |

`CommandSpec::usage()`, `arg_hint()` and `arg_help()` give the palette and
`/help` their text.

## Workflows, skills and agents

- **Workflows** register their `command:` (the default is the name). Their
  arguments come from `inputs:`. An input with `positional: N` is taken by
  position; any other input is given as `name=value`. `required`, `default`
  and `enum` are enforced. An unknown key is an error.
- **Skills** register their name. Their `args:` hint (`<required> [optional]`)
  names positional arguments, and the whole argument string becomes
  `$ARGUMENTS` (`Invocation::raw`).
- **Agents** register their name as a "switch to this agent" command.

## Name collisions

A bare `/name` goes to a built-in first, then a workflow, a skill and an
agent. Within one kind, a project command shadows a global one, which
shadows a bundled one. Every contender stays reachable:

- `/global:deploy` and `/project:deploy` reach the same name in different
  scopes (also `/builtin:name` for bundled files),
- `/skill:review` and `/workflow:review` reach different kinds with the same
  name in one scope.

`CommandRegistry::collisions()` lists every collision for `harness doctor`.

## Arguments

Arguments can be positional words, `key=value` pairs and quoted values
(`"two words"`, `'literal'`, `key="a b"`, with `\"` inside double quotes).
Errors point at the problem and show the usage:

```text
`/fix` needs <issue>. Usage: /fix <issue> [level=…] [scope=…]
`fast` is not a valid mode for `/mode`: use one of ask, auto-edit, auto
unknown command `/cots`. Did you mean `/cost`? Type /help to list commands.
unterminated " quote starting at column 5: close it with "
```

## Effort

`/effort minimal|low|medium|high|xhigh|max|ultra` sets the reasoning effort of the session's
main agent for the rest of the session; subagents keep the `effort` of
their own YAML. The next request carries it, the status line shows it
after the model (`<model> · effort low · <mode>`), and the session file
keeps it, so `/resume` restores it. `/new` and switching agents start
without one. `/effort default` drops the override: the agent YAML's
`effort` applies again, or, without one, no reasoning fields are sent and
the model uses its own default. Typed while the agent works, the change
is queued and applies after the turn.

A bare `/effort` opens a panel with the agent, the model, the active
effort and where it comes from (this session, the agent YAML or the model
default) and how the model's provider receives it, with a `default`
button and one button per level the model takes (click them, or
Tab/arrows and Enter; Esc closes). The panel updates when you pick one.

Models take different levels. A level the model does not take is sent as
the nearest one it does: the highest level below it, or else the lowest
above it. `/effort` then says so (`effort ultra: sent as max`), the panel
shows `ultra (sent as max)` and the status line `effort ultra→max`. The
same applies to an agent YAML's `effort` and to subagents.

| Provider | Levels | Sent |
|---|---|---|
| Anthropic, Claude 4.7 and later | low, medium, high, xhigh, max | `thinking: {type: adaptive}` plus `output_config.effort` ([details](providers-anthropic.md#effort-and-thinking-budget)) |
| Anthropic, Claude 4.6 | low, medium, high, max | the same |
| Anthropic, budget models (Claude Haiku 4.5) | low to max | `thinking.budget_tokens`: low 2,048, medium 8,192, high 24,576, xhigh 32,768, max 49,152 |
| OpenAI, Sign in with ChatGPT | what the plan's model list reports per model (for example low to ultra) | `reasoning.effort` ([details](providers-openai.md)) |
| OpenAI, API key | GPT-5: minimal to high; GPT-5.1: low to high; GPT-5.2 and later: low to xhigh; o-series: low to high | `reasoning.effort` on reasoning models |
| OpenRouter | minimal, low, medium, high, xhigh | `reasoning.effort` ([details](providers-openrouter.md)); OpenRouter maps it to the upstream model |

Where a provider's live model list states the levels (Anthropic's
`capabilities.effort`, ChatGPT's `supported_reasoning_levels`), the list
wins over the table above.

On a model that does not reason (for example `openai/gpt-4o`, an Anthropic
model outside the table, or an OpenRouter model whose catalog entry lacks
`reasoning`), `/effort` warns that nothing is sent and the status line
shows `no reasoning`. The choice is kept and applies after `/model`
switches to a model that reasons. The TASK-161.7 measurements of what
effort costs and saves are in
[performance.md](performance.md#fast-mode-nitro-reasoning-and-effort-task-1617).

## Hot reload

After `Catalog::reload`, call `CommandRegistry::reload(&catalog)`. It returns
true when any command was added, removed or changed. Push the new list to
completion and the palette when that happens.
