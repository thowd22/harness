---
id: doc-3
title: YAML Spec - Prompts Agents Workflows Skills
type: specification
created_date: '2026-10-22 14:26'
updated_date: '2026-10-23 02:50'
---
# YAML Spec: Prompts, Agents, Workflows; Skills (Markdown)

## Discovery & precedence

| Scope | Location | Loaded when |
|---|---|---|
| Built-in | compiled into the binary | always |
| Global | `~/.config/harness/{prompts,agents,workflows,skills}/` | always |
| Project | `<repo>/.harness/{prompts,agents,workflows,skills}/` | only after the project is **trusted** |

Later scopes shadow earlier ones by `name`; shadowing is reported in `harness doctor`. On slash-command collisions both remain reachable as `/global:name` and `/project:name`. Project YAML may only **tighten** policy (tools, paths, bash rules, budgets). Files hot-reload on change; invalid files keep the previous version and show an error with file:line:col.

`harness schema` emits JSON Schema for editor completion.

## System prompt (`prompts/*.yaml`)

```yaml
name: coder
description: Default coding prompt
layers:                       # concatenated in order; order is stable for caching
  - builtin: identity         # built-in harness identity layer (compiled in)
  - file: base.md             # relative to this yaml
  - text: |
      You are working in {{ cwd }} on {{ os }}. Today is {{ date }}.
      You may edit files matching the task scope and add or update tests.
vars: [cwd, os, date, git_branch]   # allowed template vars (no secrets, no env)
```

A layer is exactly one of `text:`, `file:` or `builtin:`. The only built-in layer is `identity` (`harness_config::prompts::IDENTITY`, at most 120 tokens): it tells the model it runs inside harness, names the harness features and asks it to read docs/guide/ and backlog/docs/ (locally, or at https://github.com/thowd22/harness) before answering questions about them. Every built-in prompt starts with it, so it sits in the stable cached prefix; custom prompts include it explicitly. `[general] identity_prompt = off` removes it from every prompt.

## Agent (`agents/*.yaml`)

```yaml
name: implementer
description: Writes code for a scoped task
model: sonnet                 # alias or provider/model — explicit, no hidden routing
effort: medium                # provider-mapped reasoning effort
prompt: coder                 # prompt name, or inline `prompt_text:`
tools: [read, write, edit, bash, todo]   # todo = shared run task list shown in the goal bar
subagents: [researcher, reviewer]   # enables spawn_agent restricted to these
memory: read-write            # off | read | read-write
max_turns: 60
budget: { usd: 2.00, tokens: 2_000_000 }
scope: ["src/**", "tests/**"]       # edits elsewhere are flagged/asked
policy:                       # tighten-only
  bash.deny: ["git push*"]
verify: cargo test -p core    # overrides [verify] command
runs_on: local                # local | <remote name> | { labels: [gpu], min_mem_gb: 32 }
output_schema:                # optional structured result for parents/workflows
  type: object
  properties: { summary: { type: string }, files_changed: { type: array } }
```

## Workflow (`workflows/*.yaml`)

GitHub-Actions-like. Each step is a node in the agent map.

```yaml
name: fix-bug
description: Reproduce, fix and prove a bug fix
command: fix                  # slash command: /fix (defaults to name)
inputs:
  issue: { type: string, required: true, positional: 0 }
  scope: { type: list, default: ["src/**", "tests/**"] }
budget: { usd: 5 }
goal: "Fix: {{ inputs.issue }}"   # shown at the top of the goal bar
tasks:                        # optional; seeds the goal bar, agents may add more via the todo tool
  - { id: repro, title: "Reproduce with failing test", step: repro }
  - { id: fix,   title: "Fix the bug",                 step: fix }
  - { id: proof, title: "Bug-fix proof",               step: proof }
steps:
  - id: repro
    agent: implementer
    prompt: "Write a failing test that reproduces: {{ inputs.issue }}. Do not fix it yet."
    scope: ["tests/**"]
    outputs: [test_name]

  - id: prove-fail
    needs: [repro]
    verify: { command: "cargo test {{ steps.repro.outputs.test_name }}", expect: fail }

  - id: fix
    needs: [prove-fail]
    agent: implementer
    scope: "{{ inputs.scope }}"
    prompt: "Make {{ steps.repro.outputs.test_name }} pass with a minimal change."
    retries: 1

  - id: proof
    needs: [fix]
    type: bugfix-proof        # pass-after, revert fix → fail, restore
    test: "{{ steps.repro.outputs.test_name }}"

  - id: review
    needs: [proof]
    agent: reviewer
    model: opus               # independent model for checking
    loop: { until: "{{ steps.review.outputs.verdict == 'approve' }}", max_iterations: 3, retry_step: fix }

  - id: confirm
    needs: [review]
    type: approval
    message: "Ship this change?"
```

Step types: `agent` (default when `agent:` present), `shell` (`run:`), `verify`, `bugfix-proof`, `approval`, `workflow` (nested `uses:`). Step keys: `id`, `needs`, `if`, `for_each`/`matrix`, `loop`, `retries`, `timeout`, `on_failure: abort|continue`, `runs_on`, `scope`, `budget`, `outputs`, `artifacts`.

Goal and tasks: `goal` and `tasks` seed the goal bar. A task with `step:` takes its status and owning agent from that step automatically; agents add or update other tasks through the `todo` tool. The goal bar is hidden when a run has no tasks.

Templating: minijinja over `inputs`, `steps.<id>.outputs|status|artifacts`, `matrix`, `run` (id, dir). No environment or secret access. All references validated at load time.

## Skills (`skills/*.md` or `skills/<name>/SKILL.md`)

Plain Markdown. Frontmatter is optional:

```markdown
---
name: release-notes
description: Draft release notes from commits since the last tag
args: "[since-tag]"
model_invocable: true         # listed (name + description only) for the model
---
Collect commits since $1 (default: latest tag) and ...
$ARGUMENTS
```

Without frontmatter: name = file stem, description = first non-heading line. `/release-notes v1.2` injects the body (with `$ARGUMENTS`, `$1…`) as a user turn. Model-invocable skills appear in the system prompt as a one-line listing with their path; the model loads one with the existing `read` tool (no extra tool, minimal tokens).

## Loader details (harness-config)

- YAML is parsed with `serde_norway` (maintained serde_yaml fork). Errors are `file:line:col: message`; unknown keys are errors; integers may use `_` separators.
- Prompt templates may only use variables declared in `vars`, and `vars` must be a subset of `cwd, os, date, git_branch`. Templates have no functions or filters. A session renders its prompt once from a frozen snapshot of these variables.
- Agent `model` is optional (defaults to `[general] default_model`). Exactly one of `prompt` / `prompt_text` is required. `tools` accepts `read, write, edit, bash, todo`; `spawn_agent` is added when `subagents` is non-empty and `memory` when `memory` is not `off`. Agent `policy` keys: `bash.deny`, `bash.ask`, `paths.protected`, `outward.ask` (`bash.allow` is rejected as loosening).
- A project agent shadowing a lower-scope agent of the same name may not add tools or subagents, raise memory, max_turns or budgets, widen scope, or drop policy rules. No agent budget may exceed `[budget] run_usd`.
- Hot reload of `.harness/` files re-checks trust: an edit makes the project untrusted until reviewed again.
- Skills without frontmatter: name = file stem (directory name for `SKILL.md`), description = first non-empty non-heading line. If a body has no `$ARGUMENTS`/`$N` placeholder, given arguments are appended after a blank line.

## Workflow implementation decisions (TASK-53..61, 104, 119)

User guide: `docs/workflows.md`. Additions to the workflow keys above:

- **Budgets** take `warn_at` (fraction of the cap, default 0.8, one soft warning) and `on_exceed: stop|warn` (default stop) on both workflow and step budgets. The remaining `stop` budget is passed to agent steps as a hard limit.
- **`runs_on` selectors** also accept `min_cpus`, `min_gpus` and `fallback: local|<host>` (doc-6). Hosts must be `[remote.<name>]`; a selector that no host can match and that has no fallback is a load error.
- **Outputs.** Agent steps return declared `outputs`/`artifacts` as a JSON object at the end of their reply (validated, one correction turn). Shell steps get `stdout`, `exit_code` and declared outputs from `name=value` stdout lines. Verify: `passed`, `exit_code`, `output`. Bugfix-proof: `proven`, `phases`. Approval: `approved`, `by`, `reason`. Nested workflow: `outputs.<child step>.<name>`.
- **Artifacts** live in `<run dir>/artifacts/<step>/<name>`. Shell steps write `$ARTIFACTS_DIR/<name>`. `steps.<id>.artifacts.<name>` is the path; agents get referenced artifacts inlined into their brief (the run dir is protected). On an `approval` step, `artifacts:` lists upstream artifacts to review.
- **`bugfix-proof`** takes `keep` (globs kept when the fix is reverted, default `["tests/**"]`). The fix is the change since the checkpoint taken before the nearest upstream agent step.
- **Visibility rule:** a step may reference only upstream steps, except that steps re-run by a `loop` may read the looping step (guard with `is defined`).
- **Skips:** without `if`, a step whose needs did not all succeed is skipped; `on_failure: continue` counts as satisfied. `abort` cancels running steps and skips the rest.
- **State** is `<run dir>/workflow.json`; resume and re-run create a new run whose state records `origin {run, kind: resume|rerun, from_steps}`.
- **Nodes:** workflow node `<name>`, step nodes `<name>/<step>`, matrix/for_each instances `<name>/<step>[i]` (children of the step node), the agent loop of an agent step `<step node>.agent`.
