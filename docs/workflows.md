# Workflows

A workflow is a GitHub-Actions-like DAG of steps, written in YAML under
`workflows/` (built-in, `~/.config/harness/workflows/`, or a trusted
project's `.harness/workflows/`). Each step becomes a node in the agent map.
The engine is the `harness-workflow` crate.

```yaml
name: fix-bug
description: Reproduce, fix and prove a bug fix
command: fix                       # /fix (defaults to name)
inputs:
  issue: { type: string, required: true, positional: 0, description: "The bug" }
  scope: { type: list, default: ["src/**", "tests/**"] }
budget: { usd: 5, warn_at: 0.8, on_exceed: stop }
goal: "Fix: {{ inputs.issue }}"
tasks:
  - { id: repro, title: "Reproduce with a failing test", step: repro }
steps:
  - id: repro
    agent: coder
    prompt: "Write a failing test for: {{ inputs.issue }}"
    outputs: [test_command]
  - id: prove-fail
    needs: [repro]
    verify: { command: "{{ steps.repro.outputs.test_command }}", expect: fail }
```

## Step types

The step type comes from `type:` or from the key the step uses.

| Type | Key | What it does | Outputs |
|---|---|---|---|
| `agent` | `agent:` (+ `prompt`, `model`) | runs the agent with the rendered prompt as its brief | `summary` plus the declared `outputs` |
| `shell` | `run:` | runs `sh -c` in the project | `stdout`, `exit_code`, plus declared outputs from `name=value` lines on stdout |
| `verify` | `verify:` (string or `{command, expect: pass\|fail}`) | runs a verify command | `passed`, `exit_code`, `output` |
| `bugfix-proof` | `test:` (+ `keep`, default `["tests/**"]`) | fix in → test passes; fix reverted → test fails; fix restored | `proven`, `phases` |
| `approval` | `message:` (+ `artifacts` to review) | pauses for a human decision | `approved`, `by`, `reason` |
| `workflow` | `uses:` (+ `with:` inputs) | runs another workflow as a child | `outputs.<child step>.<name>` |
| `dream` | `with:` (`grace_days`, `max_new_lessons`, `recent_runs`, `interval_hours`) | consolidates long-term memory (the built-in `dream` workflow) | `summary`, `merged`, `pruned`, `added`, `commit` |

At the top level, `memory: off|read|read-write` caps the memory access of
every agent step (`off` turns memory off for the workflow).

An agent step that declares `outputs` or `artifacts` is asked to end its
reply with a JSON object containing them. The object is validated, and an
invalid one gets one correction turn. A missing output fails the step.

## Step keys

| Key | Meaning |
|---|---|
| `needs` | steps that must finish first (a string or a list) |
| `if` | condition; when false the step is `skipped`. Without `if`, a step whose `needs` did not all succeed is skipped too. |
| `for_each` | a list, or `{{ expr }}`; one instance per element, available as `item` |
| `matrix` | `{key: [values]}`; one instance per combination, available as `matrix.<key>` |
| `loop` | `{until, max_iterations (3), retry_step}`: after the step, evaluate `until`; if it is false, re-run from `retry_step` (or the step itself). The step fails if `until` is still false after `max_iterations`. |
| `retries` | extra attempts after a failure (each one is a `node_retry` event) |
| `timeout` | per attempt: seconds, or `30s` / `10m` / `1h` |
| `on_failure` | `abort` (default): fail the workflow, cancel running steps, skip the rest. `continue`: record the failure and let dependents run. |
| `runs_on` | `local`, a `[remote.<name>]` host, or `{labels, min_mem_gb, min_cpus, min_gpus, fallback: local\|<host>}` |
| `scope` | edit-scope globs (a list, or `{{ inputs.scope }}`) |
| `budget` | `{usd, tokens, warn_at, on_exceed: stop\|warn}` for this step |
| `outputs` | declared output names (`letters, digits, _`) |
| `artifacts` | declared artifact names; on an approval step, the upstream artifacts to review |
| `tools` | agent steps only: MCP tools added to the agent's own for this step, as `mcp:<server>/<tool>` (only `mcp:` entries; see [mcp.md](mcp.md#tools-for-agents)) |

Load errors are reported as `file:line:col` and include unknown keys, keys
that do not fit the step type, invalid types, duplicate ids, unknown
`needs`, dependency cycles (with the cycle path, such as
`a -> c -> b -> a`), a `retry_step` that is not upstream, `tasks` that name
unknown steps, unresolved template references, and, when checked against
the catalog and harness.ini, unknown agents, hosts, nested workflows and
MCP servers.

## Templates

Prompts, commands, conditions and `with:` values are minijinja templates over:

| Variable | Meaning |
|---|---|
| `inputs.<name>` | workflow inputs |
| `steps.<id>.outputs.<name>` | a finished step's outputs (lists for matrix / for_each steps) |
| `steps.<id>.status` | `succeeded`, `failed`, `skipped` or `cancelled` |
| `steps.<id>.artifacts.<name>` | path of an artifact |
| `matrix.<key>`, `item` | the current instance |
| `run.id`, `run.dir` | the run |

Environment variables, files and secrets are not available. Undefined
variables are errors, and every reference is checked at load time: a step
may only read steps upstream of it (its `needs`, transitively). One
exception: steps that a `loop` re-runs may read the looping step, for
example to see a reviewer's feedback. Guard such references with
`{% if steps.review is defined %}`, because the looping step has no result
on the first pass.

## Artifacts

Artifacts are files a step hands to later steps. They live in the run
directory (`<run dir>/artifacts/<step>/<name>`), never in the repository.

- An **agent** step returns each artifact's content as a string field.
- A **shell** step writes `$ARTIFACTS_DIR/<name>`.
- A **verify** step stores its output.
- A **bugfix-proof** step stores its report.

Later steps refer to an artifact by name with
`{{ steps.<id>.artifacts.<name> }}`. Agents cannot read the run directory,
so when an agent's prompt refers to an artifact, its content (up to
32 KiB) is added to the brief. `artifacts::for_node(run_dir, node)` lists a
node's artifacts for the node detail view.

## Approval gates

An `approval` step records `approval_requested` on its node, and the node
shows as waiting for approval. The step's branch pauses until it is
answered. Use `gates::waiting_gates(&run_state)` to find waiting gates. In
the TUI the prompt comes through `ChannelGates`. Headless runs resolve
gates by flag (`HeadlessGates`):

- `--approve-all` approves every gate;
- `--approve <step>` approves one gate;
- `--deny <step>` denies one gate and wins over the other two.

A gate with no matching flag is denied, and the message names the flag to
pass. A denied gate fails its step, and `on_failure` decides what happens
next.

## Budgets

`budget:` on the workflow or on a step accepts these keys:

- `usd` and `tokens`: the caps;
- `warn_at`: a fraction of the cap (default 0.8) at which a soft warning is
  recorded once;
- `on_exceed`: `stop` (the default) or `warn`.

When a `stop` budget is reached, the workflow skips every step it has not
started and fails with `budget: ...`. When a `warn` budget is reached, a
warning is recorded and the workflow keeps going. The remaining amount of
a `stop` budget is passed to each agent step as a hard limit, together
with its own step budget. `WorkflowState.budget.badge()` gives the text for
map nodes, for example `$1.20/$5.00 24%`.

## Goal and tasks

`goal:` (templated) and `tasks:` seed the goal bar (the run's shared task
list; see [subagents.md](subagents.md)). A task with `step:` follows that
step's status (pending, in progress, blocked while a gate waits, then done,
failed or cancelled) and takes the step's node and agent as its owner.
Agents with the `todo` tool can add or update other tasks.

## Running, resuming and re-running

- Ready steps run in parallel. Each holds a scheduler slot
  (`[general] max_concurrent_agents`); approval and nested-workflow steps
  do not need one. Every step is declared `queued` at the start, so the
  map shows the whole DAG.
- Cancellation stops the running steps and marks every other step
  `cancelled`.
- After every change the executor writes the state to
  `<run dir>/workflow.json`.
  - `rerun::plan_resume(old_run_dir, &wf)` continues a crashed or
    cancelled workflow in a new run. Steps that succeeded are reused, and
    their artifacts are copied over.
  - `rerun::plan_rerun(old_run_dir, &wf, &[step])` re-runs from the
    selected steps, or from the failed steps when the list is empty. Those
    steps and everything downstream run again; everything upstream is
    reused.
- The new run's state records its origin. `rerun::lineage` lists a run's
  ancestors and `rerun::reruns_of` lists the runs derived from it, for run
  history. The run is named `<workflow> (re-run of <id>)`.

## Remote placement (`runs_on`)

Hosts exist only as `[remote.<name>]` in harness.ini. A host named in YAML
that is not there is a load error. `placement::HostPool` handles each kind
of `runs_on`:

- **Named host:** waits for a free job slot on that host (`max_jobs`).
- **Selector:** keeps the reachable hosts that have every label and meet
  the probed resource minimums (memory, CPUs, GPUs). It then picks the
  least busy one. When all matching hosts are busy, the step waits. When
  none matches, it uses `fallback`, or fails and explains why each host was
  rejected.

The chosen host is recorded on the step's node (`node_started.host`) and
passed to the step runner. Agent YAML takes the same `runs_on` forms,
including `{labels, min_mem_gb, min_cpus, min_gpus, fallback: local|<host>}`;
a selector with a `fallback` loads even when no host has its labels, and a
fallback host must exist in harness.ini. An agent step without its own
`runs_on` is placed like its agent; the step's `runs_on` (even `local`)
wins.

### What runs on the host

A placed step runs its tools on the host (`harness_workflow::remote`):

1. harness connects to the host (installing the helper if needed) and
   copies the worktree to a fresh run directory there (see
   [remote.md](remote.md#workspace-sync)).
2. The step runs:
   - **agent:** the model loop, credentials, memory, policy and approvals
     stay local; `read`, `write`, `edit` and `bash` act on the host's copy.
     The agent node (`<step>.agent`) and any subagent it spawns record the
     host and use it too. The host's `policy.bash.deny`/`policy.bash.ask`
     apply on top of the global policy.
   - **shell / verify:** the command runs in the host's copy with the
     host's own login environment. Nothing from your local environment is
     sent (no credentials, tokens or local `HOME`/`PATH`); only
     `$ARTIFACTS_DIR` is set, pointing to a directory on the host, and
     declared artifacts are copied back. Agent `bash` on the host likewise
     gets only the remote login `HOME`, `PATH`, `USER`, `SHELL` and locale
     plus the non-interactive settings (`TERM=dumb`, `PAGER=cat`, ...).
3. When the step succeeds, its changes come back and are applied to your
   worktree only if every file passes the local policy (protected paths)
   and the hygiene check (temp files, the step's `scope`) and nothing
   changed locally meanwhile. Otherwise nothing is applied, the step fails
   with the reasons, and the change set is saved as `remote-changes.patch`
   in the step's artifact directory.
4. The run directory is removed after a successful step and kept after a
   failure for inspection. Every attempt (see `retries`) gets its own run
   directory, so a retry starts from a fresh copy and never sees files or
   artifacts left by a failed attempt.

Later steps see the applied changes, so a local step can use the result of
a remote one. `bugfix-proof` and `dream` steps cannot run remotely yet; with
`runs_on` they fail and say to remove it.

Every remote operation is recorded on the step's node as a `remote_op`
event (host, operation, target, outcome, duration, and the bytes the
content cache saved) and every connection
change (connecting, connected, reconnecting, failed, disconnected) as a
`remote_connection` event, both tagged with the host.

## Slash commands

Every workflow registers `/<command>`. `commands::parse_args` maps the
arguments onto inputs:

- positional words fill `positional:` inputs in order;
- extra words join into the last positional string (so `/fix header
  parsing breaks on CRLF` needs no quotes);
- `--name value`, `--name=value` and `name=value` set any input;
- a bare `--flag` sets a bool input to true;
- list inputs take comma-separated values or repeated flags.

Required inputs that are still missing come back in `ParsedArgs::missing`
with a prompt each, so the palette can ask for them and then call
`commands::complete`. `commands::help(&wf)` generates help from the
description and inputs:

```
Reproduce, fix and prove a bug fix

Usage: /fix <issue> [--scope <list>]

Inputs:
  issue  string, required, position 1. The bug
  scope  list, default ["src/**","tests/**"]
```

## Bundled workflows

| Workflow | Command | Pattern |
|---|---|---|
| `implement-verify-review` | `/ivr <task> --test <cmd>` | implement, verify, then loop until an independent reviewer approves |
| `fix-bug` | `/fix <issue>` | failing test first (proved to fail), a minimal fix, a bug-fix proof, then a review loop |
| `plan-approve-implement` | `/plan <task> --test <cmd>` | a plan artifact you approve before any code changes, then implement and verify |

They use the built-in `coder`, `researcher` and `reviewer` agents. A
workflow with the same name in a global or project directory overrides
them. Each has an eval task in
`crates/harness-workflow/evals/workflows/suite.yaml`. The scripted versions
also run in-process in `cargo test -p harness-workflow --test bundled`.

## Embedding the executor

```rust
let runners = Runners::new(Arc::new(HeadlessGates::from_flags(true, [], [])))
    .with_agent(Arc::new(AgentStepRunner::new(subagent_runtime)))
    .with_shell(Arc::new(ShellStepRunner::new(exec.clone(), &root).with_recorder(recorder.clone())))
    .with_verify(Arc::new(verify.clone()))
    .with_proof(Arc::new(ProofStepRunner::new(verify, checkpoints.clone())))
    .with_checkpoints(Arc::new(StoreCheckpointer(checkpoints)));
let outcome = Executor::new(recorder, runners, scheduler)
    .with_library(Arc::new(workflows))     // for `uses:`
    .with_board(board)                     // goal bar
    .with_hosts(Arc::new(HostPool::from_registry(&hosts, &probes)))
    .run(&workflow, &inputs, None, &cancel)
    .await;
```
