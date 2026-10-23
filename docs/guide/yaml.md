# Prompts and agents

Definitions are YAML files in three scopes; later ones replace earlier ones
with the same `name`:

| Scope | Location |
|---|---|
| built-in | compiled in (`coder`, `researcher`, `reviewer`; see [defaults.md](../defaults.md)) |
| global | `~/.config/harness/{prompts,agents,workflows,skills}/` |
| project | `<repo>/.harness/{prompts,agents,workflows,skills}/`, only after you trust the project |

Files reload when they change; a broken file keeps its previous version and
shows the error with file:line:col. `harness schema agent` prints a JSON
Schema for editor completion.

## A prompt

`~/.config/harness/prompts/terse.yaml`:

```yaml prompt
name: terse
description: Short answers, small diffs
layers:
  - builtin: identity    # optional: harness's identity layer, as in the built-in prompts
  - text: |
      You are a coding agent in {{ cwd }} on {{ os }}. Today is {{ date }}.
      Keep answers short. You may edit files the task needs and add tests for them.
vars: [cwd, os, date]
```

`- builtin: identity` adds the short layer every built-in prompt starts
with: it tells the model it runs inside harness and to read the harness docs
before answering questions about harness features. Leave it out, or set
`[general] identity_prompt = off`, to send only your own text.

Only `cwd`, `os`, `date` and `git_branch` exist as template variables; no
environment variables or secrets. Say what the agent may do: prompt lint
rejects blanket prohibitions such as "do not modify the tests" (put hard
limits in `scope:` or `policy:`, which are enforced in code).

## An agent

`~/.config/harness/agents/fixer.yaml`:

```yaml agent
name: fixer
display_name: Test Fixer      # optional; shown in the palette, map and history
description: Fixes failing tests in src/
prompt: terse
model: sonnet
effort: medium
tools: [read, write, edit, bash, todo]
memory: read
max_turns: 40
budget: { usd: 2.00, tokens: 2_000_000 }
scope: ["src/**", "tests/**"]
policy:
  bash.deny: ["git push*"]
verify: cargo test
```

`tools` picks from `read, write, edit, bash, todo`, plus MCP tools as
`mcp:<server>/<tool>` (for example `mcp:github/create_issue` or
`mcp:db/*`) from servers defined in harness.ini (see
[MCP servers](../mcp.md#tools-for-agents)); `memory` is `off`,
`read` or `read-write`; `subagents: [researcher]` lets it delegate. Project
agents may only tighten what a global or built-in agent of the same name
allows.

Run it with `harness run --agent fixer "..."` or from the palette.
