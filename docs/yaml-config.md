# Prompts, agents, workflows and skills

harness reads authored configuration from YAML (prompts, agents, workflows)
and Markdown (skills). The full format is in
`backlog/docs/doc-3 - YAML-Spec-Prompts-Agents-Workflows-Skills.md`. This page
covers how the files are found and checked, and what errors look like.

## Where files are found

| Scope | Location | Loaded |
|---|---|---|
| built-in | compiled into the binary | always |
| global | `$XDG_CONFIG_HOME/harness/` or `~/.config/harness/`, in `prompts/`, `agents/`, `workflows/` and `skills/` | always |
| project | `<repo>/.harness/`, same subdirectories | only when the project is trusted and unchanged since review |

- Prompts, agents and workflows are `*.yaml` or `*.yml` files directly in their directory.
- Skills are `skills/<name>.md` or `skills/<name>/SKILL.md`.
- `$HARNESS_CONFIG` moves `harness.ini` but not the global directory.
- Symlinked directories are never followed. In a project's `.harness/`,
  symlinked files are skipped as well, because trust only covers the link
  itself, not the file it points to.

### Shadowing

Definitions are identified by their `name:` field, not by file name. A later
scope replaces an earlier one with the same name (built-in, then global, then
project). `harness doctor` lists every shadowed name, for example:

```text
prompt `coder` from /repo/.harness/prompts/coder.yaml (project) shadows builtin:prompts/coder.yaml (builtin), /home/me/.config/harness/prompts/coder.yaml (global)
```

When two skills share a name, `/name` runs the one from the highest scope.
`/global:name` and `/project:name` run a specific one. Two files in the same
scope with the same name are an error, and the first file (in sorted order)
is used.

### Hot reload

Saving a file reloads it, so you do not need to restart. If the edited file
is invalid, the previous version stays in use and the error names the file,
line and column:

```text
/home/me/.config/harness/agents/impl.yaml:3:1: unknown field `max_turnz`, expected one of `name`, ... (keeping the previous version)
```

Editing a file under `.harness/` changes the project's trust fingerprint. The
project scope then drops out until you review and trust the project again.
This is deliberate: the repository can propose config, but only you can
accept it.

## Errors

Every YAML error is reported as `file:line:col: message`. Unknown keys are
errors, so typos cannot quietly change behaviour. Numbers may use `_`
separators (`tokens: 2_000_000`).

Mistakes that involve other files are reported at load time, too: unknown
model aliases or providers, unknown prompt or subagent names, unknown remotes
or labels in `runs_on`, and agent budgets above `[budget] run_usd`.

## Prompts

- `layers` are joined in order with one blank line between them. Each layer
  is either `text:`, `file:` (a path relative to the YAML file; absolute
  paths and `..` are rejected) or `builtin:`, text compiled into harness.
  The only built-in layer is `identity`, which names harness and points the
  model to its docs; the built-in prompts start with it, and a custom
  prompt can too (`- builtin: identity`). `[general] identity_prompt = off`
  removes it from every prompt.
- Templates can use only the variables listed in `vars`, and `vars` can list
  only `cwd`, `os`, `date` (UTC, `YYYY-MM-DD`) and `git_branch`. A template
  that uses any other variable fails to load. Environment variables and
  secrets are never available, and no template functions or filters exist.
- A session renders its prompt once, when it starts. The system prompt is
  then byte-identical on every turn (which keeps provider prompt caching
  working), even if you edit the YAML during the session.

## Agents

- `display_name:` is optional: the name people see in the palette, map,
  history, goal bar, subagent cards and `harness runs show`. Without it the
  agent shows as its name with `-`/`_` as spaces and a capital first letter
  (`bugfix-proof` → `Bugfix proof`). An empty value is an error.
- `description:` is one line shown in the palette and to parents choosing a
  subagent. Without it the palette says `Run the <display name> agent`.
- Set exactly one of `prompt:` (the name of a prompt) or `prompt_text:`
  (inline text, not templated).
- `model:` is optional. Without it the agent uses `[general] default_model`.
- `tools:` can list `read`, `write`, `edit`, `bash`, `grep`, `glob` and
  `todo` (see [tools.md](tools.md)). The model
  sees only these tools, in the order you list them. In addition,
  `spawn_agent` appears when `subagents:` is not empty, and `memory` appears
  when `memory:` is `read` or `read-write`. No other tool is ever offered.
- `mode:` (`ask`, `auto-edit` or `auto`) sets how much this agent may do
  without asking. It can only make the session's mode stricter: an agent
  with `mode: ask` asks before every edit even in an `auto` session, while
  `mode: auto` in an `ask` session changes nothing.
- `policy:` can only add restrictions. The allowed keys are `bash.deny`,
  `bash.ask`, `paths.protected` and `outward.ask`, each taking a string or a
  list. `bash.allow` is rejected because allow rules belong in
  `harness.ini`.
- A project agent that shadows a global or built-in agent of the same name
  can only tighten it. It cannot add tools or subagents, raise memory access,
  `max_turns` or budgets, widen `scope`, or drop policy rules.

## Skills

- Frontmatter (`name`, `description`, `args`, `model_invocable`) is optional.
  Without it, the name is the file stem (or the directory name for
  `SKILL.md`), and the description is the first non-empty line that is not a
  heading.
- `/name args` sends the body as a user turn. `$ARGUMENTS` becomes the whole
  argument string, `$1` to `$9` become positional arguments (quotes group
  words, as in a shell), and `$$` becomes a literal `$`. If the body has no
  placeholder and you pass arguments, they are appended after a blank line.
- Skills with `model_invocable: true` are listed in the system prompt, one
  line each with their path. The model opens one with the `read` tool when
  it is relevant.

## `harness schema`

`harness schema [prompt|agent|workflow]` prints the JSON Schema (draft
2020-12) for that file type, or all three as one object keyed by kind. Use it
for editor completion. With yaml-language-server, for example:

```yaml
# yaml-language-server: $schema=/path/to/agent.schema.json
name: implementer
```

## `harness lint`

`harness lint` checks every active prompt, agent and skill. It also reports
files that fail to load and broken references. It exits 1 if there are
errors, otherwise 0. `harness doctor` shows the same findings, including
warnings.

| Rule | Severity | Flags |
|---|---|---|
| `blanket-prohibition` | error | "Do not modify the tests", "never change config", a bare "DO NOT MODIFY". The fix it suggests is to say what *is* allowed, and to enforce hard limits with `scope:` or `policy:` |
| `contradiction` | error | One source forbids what another requires or allows ("never run git commit" against "always run git commit"); a prompt forbids a tool the agent has; a prompt tells the model to use a tool the agent lacks |
| `token-budget` | warning | A prompt, inline `prompt_text` or the skill listing estimated above 4000 tokens (bytes / 4) |
| `invalid` | error | Load and reference errors |

Findings in memory text are warnings only: memory is data, not authored
config.

```text
/home/me/.config/harness/prompts/base.md:12:1: error [blanket-prohibition]: prompt `coder` layer 1: blanket prohibition "Do not modify the tests" over `tests`
  help: say what is allowed instead, e.g. "You may add or update tests for the code you change; keep existing assertions unless the task says they are wrong." Hard limits belong in the agent's `scope:` or `policy:` (enforced in code), not in prose
1 error, 0 warnings
```

## Library entry points

- `harness_config::discovery::{Roots, Catalog, Watcher}`: discovery, layering and reload.
- `harness_config::yaml::{parse, YamlError, run_schema}`: located parsing and `harness schema`.
- `harness_config::prompts::{Prompt, PromptVars, SessionPrompt}`.
- `harness_config::agents::{Agent, ValidationContext, validate}`.
- `harness_config::skills::{Skill, listing, resolve_command}`.
- `harness_config::lint::{lint_catalog, run_lint, doctor_lines}`.
