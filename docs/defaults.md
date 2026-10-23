# Built-in defaults

harness ships a small set of prompts, agents and one workflow inside the
binary (`harness_config::builtins::BUILTINS`, files under
`crates/harness-config/assets/`). They are the lowest discovery scope, so a
file with the same `name` in `~/.config/harness/` or a trusted project's
`.harness/` replaces them.

| Kind | Name | What it is |
|---|---|---|
| agent | `coder` | The default agent. Tools: `read`, `write`, `edit`, `bash`, `grep`, `glob`, `todo`. Memory: `read-write`. |
| agent | `researcher` | Read-only investigation: `read`, `bash` (commits, pushes and `rm` denied), `grep`, `glob`, memory `read`, low effort. |
| agent | `reviewer` | Independent review: `read`, `bash` (commits and pushes denied), `grep`, `glob`, no memory, high effort. |
| prompt | `coder` | The identity layer + `base.md` + `coder.md` + `tools.md` + `memory.md` + `context.md` + a line with cwd, OS, branch and date. |
| prompt | `researcher`, `reviewer` | The identity layer, one inline layer, `tools.md` and `context.md`. |
| workflow | `dream` | Memory consolidation settings (see [dreaming.md](dreaming.md)). |

## What the defaults encode

The defaults are short and say what the agent may do rather than listing
prohibitions (prompt lint rejects blanket "do not modify" rules). They encode
the known fixes:

- **Verify before claiming done.** "A task is done only when the checks you
  ran pass; if a check fails or you could not run it, say so plainly."
- **No reflexive broad staging.** "Stage files by name (`git add
  path/to/file`) when you commit, and commit only when the user asks."
- **Minimal changes.** "Keep each change as small as the task allows."
- **Clean up.** "Put scratch files ... in a temporary directory, and remove
  any you created in the project before you finish."
- **Plans for multi-step work.** The coder prompt explains when to use the
  `todo` tool (several steps) and when not to (one-step tasks).
- **Know harness.** Every built-in prompt starts with the identity layer
  (`- builtin: identity`, `harness_config::prompts::IDENTITY`, at most 120
  tokens, compiled into the binary). It says the model runs inside harness,
  names the harness features (skills, workflows, agents, subagents, memory,
  dreaming, context, compaction, agent map, goal bar, slash commands,
  command palette, harness.ini, remotes) and asks it to read `docs/guide/`
  (usage) and `backlog/docs/` (design) before answering questions about
  them: the local copies when the working directory is the harness
  repository, otherwise https://github.com/thowd22/harness. It is the first
  layer, so it sits in the cached prefix. `[general] identity_prompt = off`
  removes it; a custom prompt includes it with `- builtin: identity`.
- **Focused, parallel tool use.** `tools.md` (all three prompts) says to
  search with `grep`/`glob` and read with `read` (bash for builds, tests and
  git), to make independent tool calls in the same turn, and that the bash
  working directory persists, so commands need no `cd <dir> &&` prefix.
- **Memory.** Agents with `memory: read-write` include `prompts/memory.md`,
  which is exactly `harness_memory::extract::PROMPT_SECTION` (a test keeps
  them identical).

## Overriding a default

Copy the file into your global config directory and edit it:

```sh
mkdir -p ~/.config/harness/agents
harness schema agent > ~/.config/harness/agent.schema.json
```

```yaml agent
name: coder
description: My coder, pinned to a model with a turn limit
prompt: coder
model: sonnet
tools: [read, write, edit, bash, grep, glob, todo]
memory: read-write
max_turns: 40
```

Keep the tools the prompt mentions: prompt lint reports a prompt that refers
to a tool (here `todo`, `edit`, `write`, `bash`) the agent does not have.

`harness doctor` lists which definitions shadow which.

## Guarantees checked by tests

- Every default loads without errors and passes prompt lint with the default
  config (`builtins::tests`).
- Rendered prompts stay well inside the initial context budget (a quarter of
  `LintOptions::prompt_token_budget`).
- The identity layer is the first layer of every built-in agent's prompt,
  stays at most 120 tokens and names every key term (`prompts::tests`,
  `builtins::tests`); the `identity` eval suite checks that a feature
  question leads to reading the docs ([evals.md](evals.md)).
- The rendered prompts and the request each provider would send for every
  default agent are golden snapshots ([golden-snapshots.md](golden-snapshots.md)).

## Changing a default

Defaults change behaviour for most users, so changes must be deliberate:

1. Edit the asset (or `prompts::IDENTITY`, which the fingerprint also
   covers) and accept the changed golden snapshots
   (`cargo insta review`).
2. Add an entry to the `## Defaults` section of `CHANGELOG.md` that says what
   changed and why, ending with `defaults-fingerprint: <hex>`.
3. `cargo test -p harness-eval --test defaults_changelog` fails until the
   entry names the current fingerprint, and prints the value to use.

The fingerprint (`harness_eval::defaults::fingerprint`) covers every built-in
file and the `harness.ini` defaults, so a changed default model or effort
needs an entry too. Run the starter evals against a baseline before shipping
([evals.md](evals.md)).
