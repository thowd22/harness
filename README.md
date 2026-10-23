# harness

[![CI](https://github.com/thowd22/harness/actions/workflows/ci.yml/badge.svg)](https://github.com/thowd22/harness/actions/workflows/ci.yml)

**A fast, terminal-native coding-agent harness, written in Rust with [ratatui](https://ratatui.rs).**

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/thowd22/harness/main/install.sh | bash
```

Installs the latest release for Linux (x86_64, aarch64) or macOS (arm64, x86_64) into `~/.local/bin`, after checking its sha256. No sudo. [Read the script first](install.sh) if you like; it is short. Pin a version with `HARNESS_VERSION=0.2.0`, pick a directory with `HARNESS_INSTALL_DIR=~/bin`, and re-run it to upgrade. To uninstall, `rm ~/.local/bin/harness` (and `~/.config/harness` for your config). Other platforms: `cargo install --git https://github.com/thowd22/harness --locked harness`. More in [the install guide](docs/guide/install.md).

Research and real-world bug reports agree: most failures of AI coding tools come from the *harness* (tool plumbing, context handling, permissions, verification, change hygiene and instructions), not the model. harness treats the model as interchangeable and makes those parts the product.

> **Status: under active development.** Work is planned and tracked in [`backlog/`](backlog/) with [Backlog.md](https://github.com/MrLesk/Backlog.md). Run `backlog board` to see what is done and what is in progress.

## Highlights

- **Claude, OpenAI and OpenRouter** behind one normalized provider layer, with streaming, tool calls, reasoning, prompt caching and retries. Add API keys with `harness login` or `/login`, or use a ChatGPT Plus or Pro plan with **Sign in with ChatGPT**. `/model` opens a picker listing every model your credentials can use.
- **YAML-defined prompts, agents, subagents and workflows.** Workflows look like GitHub Actions (`needs`, `if`, `matrix`, `loop`, retries, approval gates) and run as `/slash` commands.
- **Markdown skills.** Drop a `.md` file into `skills/` and it becomes `/skill-name`.
- **An agent map.** A live graph of every agent and workflow step, with node logs in the style of GitHub Actions, real-time output and reasoning, a request inspector and run replay.
- **A goal bar.** An opencode-style task pane showing the goal, each task's status and which agent is working on it. It only appears when tasks exist.
- **Local memory, modeled on Devin.** A git-backed Markdown "memory drive" with a nightly *Dreaming* pass that cleans it up.
- **SSH remotes.** Send an agent's tools to a bigger box with `runs_on:`. Model calls and credentials stay local. Traffic to the helper is zstd-compressed, repeated file contents are sent once, and SSH's own compression is skipped on fast links.
- **Verification and hygiene enforced in code.** Verify runs before and after a change, the agent can't declare "done" until verify passes, bug fixes can be proven to fail before and pass after, an independent reviewer can check the work, checkpoints back `/undo`, and broad `git add -A` is blocked.
- **Safe by default.** Permission rules are checked in code. A repo's project config only loads after you trust it, can only tighten rules, never loosen them, and agents cannot read the credentials file. There is a Landlock sandbox and secret redaction everywhere.
- **Simple, fast UI.** A Dracula theme, working indicators, timestamps, detailed tool-call cards, and a few keys plus one command palette. Everything on screen is clickable *and* keyboard-operable.
- **One config file.** `harness.ini` holds everything, including credentials and keybinds.

## Quick start

```sh
cargo install --path crates/harness --locked

mkdir -p ~/.config/harness
cat > ~/.config/harness/harness.ini <<'EOF'
[general]
default_model = sonnet

[provider.anthropic]
api_key_env = ANTHROPIC_API_KEY
EOF
chmod 600 ~/.config/harness/harness.ini   # required: the file may hold credentials

harness login anthropic   # optional: check and save a key instead of api_key_env (openai also offers Sign in with ChatGPT)
harness config check   # validate the config (errors include file:line)
harness doctor         # credentials, connectivity, sandbox, remotes, prompt lint
harness                # start the TUI in the current project
```

Try the UI without an API key:

```sh
harness ui-demo        # scripted session through the fake provider (--gallery: every component)
```

## Using it

### Keys

| Key | Action |
|---|---|
| `Enter` | send / activate |
| `Shift+Enter`, `Alt+Enter` | newline |
| `Esc` | cancel / back |
| `Tab` | switch Chat ↔ Map |
| `Ctrl+P` | command palette (everything else lives here) |
| `Ctrl+C` | interrupt the running agent |
| `Ctrl+Q` | quit |

Change any of them in `[keys]` in `harness.ini`. The mouse works on everything visible; set `[ui] mouse = false` to get native terminal text selection back.

### Slash commands

Built-ins include `/help`, `/new`, `/resume`, `/rename`, `/model`, `/login`, `/mode`, `/agents`, `/workflows`, `/skills`, `/memory`, `/runs`, `/remote`, `/cost`, `/compact`, `/undo`, `/diff`, `/context`, `/config`, `/doctor` and `/quit`. Every workflow and skill adds its own command. See [docs/commands.md](docs/commands.md).

### Command line

| Command | Purpose |
|---|---|
| `harness` | interactive TUI |
| `harness run "<prompt>" [--json]` | headless agent run; `--json` streams redacted events |
| `harness wf run <workflow> [args]` | run a workflow headless |
| `harness runs show <id> [--json]` | summarize a run |
| `harness eval run <suite.yaml>` | run an eval suite |
| `harness login <provider>` / `harness logout <provider>` | save and check an API key, or Sign in with ChatGPT; remove saved credentials |
| `harness config check \| show` | validate the config, or show the effective config |
| `harness lint` | prompt lint (blanket prohibitions, contradictions, size) |
| `harness schema` | JSON Schema for prompt, agent and workflow YAML |
| `harness doctor` | diagnose configuration and environment |
| `harness ui-demo [--gallery\|--print]` | a scripted session without a model, or the component gallery |
| `harness sessions` | saved sessions by name and last activity |

Every command is described in [docs/cli.md](docs/cli.md).

## Configuration at a glance

| What | Where |
|---|---|
| Everything user-owned: providers, keys, models, keybinds, UI, policy, budgets, memory, remotes | `~/.config/harness/harness.ini` |
| Global prompts, agents, workflows, skills | `~/.config/harness/{prompts,agents,workflows,skills}/` |
| Project prompts, agents, workflows, skills (loaded only after you trust the project) | `<repo>/.harness/…` |

A workflow, for flavour:

```yaml
name: fix-bug
command: fix
inputs:
  issue: { type: string, required: true, positional: 0 }
goal: "Fix: {{ inputs.issue }}"
steps:
  - id: repro
    agent: implementer
    prompt: "Write a failing test that reproduces: {{ inputs.issue }}"
  - id: fix
    needs: [repro]
    agent: implementer
    runs_on: { labels: [gpu], fallback: local }
  - id: review
    needs: [fix]
    agent: reviewer
    model: opus
```

Run it with `/fix "header parsing drops the last field"`.

## Documentation

- **User guide:** [docs/guide/](docs/guide/README.md) covers install, harness.ini, YAML, skills, workflows, memory, remotes and the palette. Every example in it is checked by a test.
- **Area docs:** [docs/](docs/) covers providers, tools, policy, hygiene, verification, context, cost, sessions, visibility and more.
- **Design specs:** [backlog/docs/](backlog/docs/) has the architecture and problem map, the harness.ini reference, the YAML spec, memory, the agent map, SSH remotes and the visual design.

## Development

```sh
cargo build --workspace
cargo test --workspace                                 # offline: fake provider, temp dirs, local mocks
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo deny check                                       # needs cargo-deny
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, tests and cargo-deny on every push and pull request, on Linux and macOS. Live-provider tests are `#[ignore]`d or gated behind an environment variable, and tests that need host features (Landlock, bwrap, sshd) check the degraded path instead and log why. See [docs/ci.md](docs/ci.md).

### Workspace layout

| Crate | Responsibility |
|---|---|
| `harness-config` | `harness.ini`, secrets, YAML discovery and layering, trust store, prompt lint |
| `harness-events` | Event schema, run store (JSONL + blobs), redaction, reducers |
| `harness-providers` | Normalized messages, `Provider` trait, Anthropic / OpenAI / OpenRouter / Fake, registry, pricing |
| `harness-tools` | `Workspace`/`Exec` traits, local implementation, read/write/edit/bash, output spill |
| `harness-policy` | Rules engine, shell parsing, protected paths, approvals, sandbox |
| `harness-agent` | Agent loop, tool dispatch, subagents, todo, context, compaction, budgets, commands |
| `harness-workflow` | Workflow schema, templating, DAG executor, gates, artifacts, re-runs |
| `harness-hygiene` | Change ledger, checkpoints, scope, hygiene report, verify, bug-fix proof, review |
| `harness-memory` | Memory drive, search, memory tool, extraction, dreaming |
| `harness-remote` | SSH transport, remote helper protocol, remote workspace, sync, probes |
| `harness-tui` | ratatui app: chat, map, detail, goal bar, palette, approvals, diff, theme |
| `harness-eval` | Eval suites, runner, baselines, doctor |
| `harness` | The binary that wires it all together |

Contributing, by hand or with a coding agent: read [AGENTS.md](AGENTS.md) first.

## License

MIT
