# The `harness` command

| Command | What it does | Exit codes |
|---|---|---|
| `harness [--resume ID\|last] [--agent NAME] [--model REF] [--mode M] [--cwd DIR]` | the interactive TUI in the project | 0, 1 on startup errors |
| `harness run [OPTIONS] PROMPT` | one headless agent run ([headless.md](headless.md)) | 0 ok, 1 error, 2 usage, 3 verify failed, 4 budget, 5 max turns, 130 cancelled |
| `harness wf list` | every workflow with its slash command | 0 |
| `harness wf help NAME` | a workflow's inputs and steps | 0, 1 unknown |
| `harness wf run NAME [OPTIONS] [ARGS]` | a workflow, headless ([workflows.md](workflows.md)) | 0 succeeded, 1 failed, 2 usage, 130 cancelled |
| `harness eval run SUITE` / `eval compare BASE CUR` | eval suites and baselines ([evals.md](evals.md)) | 0, 1 regression or error, 2 usage |
| `harness login <anthropic\|openai\|openrouter> [--api-key-cmd CMD\|--api-key-env VAR]` | check an API key with the provider and save it to harness.ini (mode 0600) ([providers.md](providers.md#logging-in)) | 0, 1 rejected or failed, 2 usage |
| `harness login openai --chatgpt` | Sign in with ChatGPT in the browser; tokens go to `<data_dir>/auth` (0600) and `auth = chatgpt` is set ([providers-openai.md](providers-openai.md)) | 0, 1 denied or failed, 2 usage |
| `harness logout <provider> [--yes]` | remove the provider's `api_key`, `api_key_cmd`, `api_key_env` and `auth` after confirmation; for openai also delete the ChatGPT tokens | 0, 1 cancelled, 2 usage |
| `harness doctor` | every health check with fixes ([doctor.md](doctor.md)) | 0, 1 when a check fails |
| `harness models [--all] [--json] [--refresh\|--offline]` | usable models from every provider, with prices, capabilities and auth method ([models.md](models.md#harness-models)) | 0, 1 config error, 2 usage |
| `harness lint` | prompt lint over prompts, agents, skills and the memory index | 0, 1 on errors |
| `harness config check [PATH]` | validate harness.ini | 0, 1 |
| `harness config show [PATH]` | the effective, redacted config with the source of each value ([effective-config.md](effective-config.md)) | 0, 1 |
| `harness schema [prompt\|agent\|workflow]` | JSON Schema of the YAML files | 0, 2 unknown kind |
| `harness runs show ID\|PREFIX\|last [--json]` | a run as a tree, or its reducer state as JSON | 0, 1, 2 usage |
| `harness sessions` | saved sessions, newest first | 0 |
| `harness ui-demo [--gallery\|--print]` | a scripted session without a model (below) | 0, 1 |
| `harness remote-agent --stdio` | the remote helper, started over SSH ([remote.md](remote.md)) | |
| `harness --version` | version and git commit, e.g. `harness 0.1.0 (1a2b3c4d5e6f)` ([releasing.md](releasing.md#version-and-commit)) | 0 |

Every command reads harness.ini (`$HARNESS_CONFIG`, else
`~/.config/harness/harness.ini`), opens the data directory and logs to
`<data_dir>/logs` with credentials redacted. `remote-agent` and the
internal sandbox launcher run before any of that.

## `harness wf run`

```
harness wf run NAME [--input NAME=VALUE]... [--json]
               [--approvals deny|allowlist [--allow PATTERN]...] [--mode M]
               [--model REF] [--max-turns N] [--budget-usd X]
               [--approve-all | --approve STEP... | --deny STEP...]
               [--resume RUN | --rerun RUN [--from STEP]...]
               [--cwd DIR] [--fake-script FILE] [ARGS]...
```

`NAME` is a workflow name or its slash command (`fix-bug` or `fix`).
`ARGS` are parsed like the slash command's arguments; `--input` sets any
input by name. Missing required inputs are an error that shows the usage.
Approval steps are denied unless approved with `--approve-all` or
`--approve STEP` (`--deny` wins). `--resume` takes over the finished steps of
an earlier run; `--rerun` runs the given steps (default: the failed ones)
and everything downstream again. Headless workflows follow
`[memory] headless_access` like `harness run`.

## `harness ui-demo`

Creates a throwaway project and data directory and runs the real TUI and
agent loop against a scripted fake model: streaming reasoning and text, a
todo list in the goal bar, a researcher subagent, an edit with its diff, a
provider rate-limit retry, a bash command that asks for approval (answer
with `y`), the map (Tab) and the palette (Ctrl+P). `--gallery` shows each
component on its own page instead; `--print` plays the session without a
terminal and prints the chat, map and palette screens as text.
