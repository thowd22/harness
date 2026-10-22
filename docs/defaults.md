# Built-in defaults

harness ships a small set of prompts, agents and one workflow inside the
binary (`harness_config::builtins::BUILTINS`, files under
`crates/harness-config/assets/`). They are the lowest discovery scope, so a
file with the same `name` in `~/.config/harness/` or a trusted project's
`.harness/` replaces them.

| Kind | Name | What it is |
|---|---|---|
| agent | `coder` | The default agent. Tools: `read`, `write`, `edit`, `bash`, `todo`. Memory: `read-write`. |
| agent | `researcher` | Read-only investigation: `read`, `bash` (commits, pushes and `rm` denied), memory `read`, low effort. |
| agent | `reviewer` | Independent review: `read`, `bash` (commits and pushes denied), no memory, high effort. |
| prompt | `coder` | `base.md` + `coder.md` + `memory.md` + a line with cwd, OS, branch and date. |
| prompt | `researcher`, `reviewer` | One inline layer each. |
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
tools: [read, write, edit, bash, todo]
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
- The rendered prompts and the request each provider would send for every
  default agent are golden snapshots ([golden-snapshots.md](golden-snapshots.md)).

## Changing a default

Defaults change behaviour for most users, so changes must be deliberate:

1. Edit the asset and accept the changed golden snapshots
   (`cargo insta review`).
2. Add an entry to the `## Defaults` section of `CHANGELOG.md` that says what
   changed and why, ending with `defaults-fingerprint: <hex>`.
3. `cargo test -p harness-eval --test defaults_changelog` fails until the
   entry names the current fingerprint, and prints the value to use.

The fingerprint (`harness_eval::defaults::fingerprint`) covers every built-in
file and the `harness.ini` defaults, so a changed default model or effort
needs an entry too. Run the starter evals against a baseline before shipping
([evals.md](evals.md)).
