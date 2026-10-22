# Headless runs: `harness run`

`harness run` runs an agent without the TUI, in the current directory. It is
what evals, scripts and CI use.

```sh
harness run "fix the failing test in src/parser.rs"
harness run --json --max-turns 20 "add a --verbose flag" > events.jsonl
```

The default agent is `coder`: the `coder` agent from your agent directories
if you defined one, otherwise a built-in agent with the `read`, `write`,
`edit` and `bash` tools. The model is `--model`, else the agent's `model`,
else `[general] default_model`.

Sign in with ChatGPT is a personal credential: with `[provider.openai]
auth = chatgpt`, `harness run` (and so `harness eval run`) and `harness wf
run` stop with an error before calling a model unless `chatgpt_headless =
true` is set, and always when `CI` is set. Use an API key there. See
[providers-openai.md](providers-openai.md#headless-runs-workflows-and-evals).

## Options

| Option | Meaning |
|--------|---------|
| `--json` | Stream every event as one JSON line on stdout. Events are redacted (credentials from harness.ini and the built-in secret patterns are masked). |
| `--agent NAME` | Agent profile. |
| `--model REF` | Model alias or `provider/model`. |
| `--approvals deny` | (default) Every call that would ask for approval is denied; the model is told why. |
| `--approvals allowlist` | Calls that would ask are allowed only if they match an `--allow` pattern (or an ini `bash.allow` rule). |
| `--allow PATTERN` | Command pattern for allowlist mode, e.g. `--allow 'cargo test*'`. Repeatable. |
| `--mode ask\|auto-edit\|auto` | Autonomy mode for this run (default `[general] mode`). |
| `--max-turns N` | Stop after N model calls. |
| `--budget-usd X`, `--budget-tokens N` | Stop when this run has spent X dollars or N tokens. |
| `--no-verify` | Skip the `[verify] command`. |
| `--resume ID\|last` | Continue a saved session (see `sessions.md`). |
| `--cwd DIR` | Project directory. |
| `--fake-script FILE` | Use the scripted fake provider (tests and demos). |

Deny rules and protected paths always apply; headless mode never asks.

Without `--json`, the final answer is printed on stdout and a one-line
summary (stop reason, turns, tokens, cost, session id) on stderr.

## Verification

When the agent ends its turn and `[verify] command` (or the agent's
`verify`) is set, the command runs with `sh -c` in the project, with secrets
removed from its environment. Its result is recorded as `verify_finished`.

## Exit codes

| Code | Meaning |
|------|---------|
| 0 | The agent finished and verification passed (or none is configured). |
| 1 | Error: provider failure, bad configuration, ... |
| 2 | Invalid command line. |
| 3 | The agent finished but verification failed. |
| 4 | A token or cost budget was exhausted. |
| 5 | `max_turns` was reached. |
| 130 | Cancelled (Ctrl+C or SIGTERM). |

## Other commands

- `harness sessions` lists saved sessions.
- `harness config check [PATH]` validates harness.ini (exit 1 on errors).
- `harness schema [prompt|agent|workflow]` prints the JSON schemas.
