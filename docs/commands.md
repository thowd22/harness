# Slash commands

Every slash command is in one registry (`harness_agent::commands::CommandRegistry`):
built-ins, workflows, skills and agent switches. Input completion and the
command palette both read from it.

## Built-ins

| Command | Arguments | What it does |
|---|---|---|
| `/help` | `[command]` | Show commands and keys, or the details of one command |
| `/new` | | Start a new session in this project |
| `/resume` | `[session]` (id, prefix or `last`) | Resume a saved session |
| `/model` | `[model]` (alias or provider/model) | Switch the model directly, or open the model picker without an argument |
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
| `/login` | `[anthropic\|openai\|openrouter]` | Add, check and save an API key in a modal, or Sign in with ChatGPT for openai ([Logging in](providers.md#in-the-tui-login)) |
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

## Hot reload

After `Catalog::reload`, call `CommandRegistry::reload(&catalog)`. It returns
true when any command was added, removed or changed. Push the new list to
completion and the palette when that happens.
