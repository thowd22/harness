# AGENTS.md

Guidance for coding agents (and humans) working on this repository. `CLAUDE.md` holds the Backlog.md workflow rules; this file covers everything else.

## What this is

`harness` is a Rust + ratatui terminal coding-agent harness. The core is headless library crates that emit typed events; the TUI, headless CLI, eval runner and remote helper all consume the same engine. Start with `backlog/docs/doc-1 - Architecture-and-Problem-Map.md` before any non-trivial change.

## Commands

```sh
cargo build -p <crate>                                  # build what you touched
cargo test -p <crate>                                   # test what you touched
cargo clippy -p <crate> --all-targets -- -D warnings    # lint what you touched
cargo fmt --all                                         # format
cargo test --workspace                                  # full suite before finishing
cargo insta review                                      # review changed snapshots (never auto-accept)
cargo run -p harness -- ui-demo                         # see UI changes with scripted data
```

The toolchain is pinned in `rust-toolchain.toml`. CI denies warnings, so a change is not finished until fmt, clippy and tests are clean.

## Where things live

| Need | Look in |
|---|---|
| Design specs (architecture, ini, YAML, memory, agent map, remote, visual design) | `backlog/docs/` |
| Tasks, acceptance criteria, Definition of Done | `backlog/tasks/` (use the `backlog` CLI) |
| User and area documentation | `docs/`, user guide in `docs/guide/` |
| Crate responsibilities | the workspace table in `README.md` |
| Built-in prompts, agents, workflows | `crates/harness-config` assets |
| Golden request snapshots | `crates/harness-eval/tests/golden_requests.rs` |

## Working on a task

1. Find the task in `backlog/` and follow the Backlog.md workflow in `CLAUDE.md`. The acceptance criteria and Definition of Done are the contract.
2. Read the design doc the task links to. If your change contradicts it, update the doc in the same change.
3. Keep the change focused on the task. Don't refactor unrelated code, add unrequested features, or leave compatibility shims nobody asked for.
4. Finish only when every acceptance criterion and DoD item is met: tests, docs, the loop guard and UI interaction.

### Loop guard

If the same test, build or check still fails after **3 fix attempts**, or you have made no measurable progress for about 30 minutes, **stop**. Never delete, skip, `#[ignore]` or weaken a test or check to get a green run. Record what you tried, the exact failing output and your best hypothesis in the task's implementation notes, leave the task In Progress, and ask for help.

## Engineering rules

These come from the design and are checked in review:

- **Enforce in code, not in prose.** Scope, protected paths, staging rules, budgets and verification belong in the harness, not in prompt text. Prompts state what *is* allowed, never blanket prohibitions; `harness lint` flags them.
- **The repo can ask; only the user can grant.** Project YAML may only tighten policy. Loosening lives only in the user's `harness.ini`.
- **Everything is an event.** New behaviour that users or evals should see emits a typed event in `harness-events`. Views are folds over events, so never feed the UI from side channels.
- **Secrets never leak.** Use the `Secret` type, route output through the redactor, scrub subprocess environments, and never log or snapshot credentials. `harness.ini`, the trust store and data-dir internals are protected paths.
- **Keep the initial context small.** The four core tools have compact schemas, and extra tools appear only when an agent enables them. CI enforces the initial-context budget.
- **Request prefixes must stay stable** for prompt caching. Don't reorder prompt layers or tool schemas casually; golden snapshots will show it.
- **UI:**
  - Use semantic theme tokens only, never raw colors.
  - Every visible interactive element registers with the interaction layer, so it is both clickable and keyboard-operable, with tests for both.
  - Render only when something changed.
- **Errors returned to the model** are short and actionable, and say how to fix the call.
- **Dependencies:** add shared versions to `[workspace.dependencies]` and use `dep.workspace = true` in the crate.
- **Public items** need rustdoc. `unsafe_code` is denied workspace-wide.

## Testing rules

- Tests run offline: use the fake provider (`harness_providers::fake`), local mock servers and temp dirs. Live-provider and real-SSH tests are `#[ignore]` or gated behind an env var.
- UI tests render to ratatui's `TestBackend` and include styled snapshots where the visual design matters. Never launch the interactive TUI in a test.
- Every acceptance criterion gets a test. Bug fixes start with a failing test.
- Snapshot changes need review with `cargo insta review`. Accept a change only when you meant to make it, and call out prompt or default changes in `CHANGELOG.md` under *Defaults*.

## Safety

- Don't commit, push, rewrite git history or stage with `git add -A` / `git add .` unless the user asks. Stage explicit paths.
- Don't start long-running servers or the interactive TUI from automation.
- Put scratch and debug files outside the repo, and remove anything temporary before finishing.
