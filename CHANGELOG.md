# Changelog

All notable changes to harness are listed here.

New entries go under `## Unreleased`. Cutting a release (the *Cut release*
workflow, `scripts/release-bump.sh`) renames that section to
`## [X.Y.Z] - YYYY-MM-DD`, and its text becomes the GitHub release notes
(`scripts/release-notes.sh`). See [docs/releasing.md](docs/releasing.md).
The `## Defaults` section at the end is separate: it records every change
to built-in prompts, agents, workflows and `harness.ini` defaults.

## Unreleased

### Added

- `harness login <provider>` and `harness logout <provider>`: masked API key
  entry (or `--api-key-cmd` / `--api-key-env`), checked with one cheap
  authenticated request before it is written to harness.ini with mode 0600.
  `harness doctor` shows the auth method in use per provider.
- `/login [provider]` in the TUI: the same login in a modal (masked key,
  keyboard and mouse), with the new key added to the redactor.
- Sign in with ChatGPT for the OpenAI provider (`harness login openai
  --chatgpt`, or method 4 of `harness login openai` and `/login openai`):
  use a ChatGPT Plus or Pro plan instead of an API key. Browser sign-in on
  127.0.0.1 with PKCE, state and nonce, a verified ID token and the
  `chatgpt.tokens.use.direct` plan scope; tokens live in the protected
  `<data_dir>/auth/` store (mode 0600) and refresh automatically.
  `[provider.openai] auth = api_key | chatgpt` selects the method; the plan
  usage limit is an error whose action is Manage usage, with no fallback to
  an API key. In the TUI the turn ends with a panel whose first, focused
  button is Manage usage (Enter or a click opens the usage page), and the
  `error` event carries the action as an optional `action: {label, url}`. Headless runs, workflows and evals refuse it unless
  `chatgpt_headless = true`, and always in CI. `harness logout openai`
  deletes the tokens.
- First public release: the ratatui TUI, headless `harness run` and `harness
  wf run`, YAML prompts, agents and workflows, Markdown skills, the agent
  map, local memory with Dreaming, SSH remotes, enforced verification and
  change hygiene, and the Claude, OpenAI and OpenRouter providers.
- Prebuilt binaries on GitHub Releases for Linux x86_64 and aarch64 (static
  musl) and macOS arm64 and x86_64, plus the static remote helpers, each
  with a `.sha256` file. Releases are only built from a commit whose full
  CI passed.
- A model catalog merges the built-in model table, `[models]` aliases and
  the live model lists of Anthropic, OpenAI and OpenRouter. It marks models
  usable only when their provider has working credentials, flags models
  without tool calling as unusable for agents, tells Sign in with ChatGPT
  plan models apart from API-key models, and caches live lists in the data
  directory so it works offline. `harness models` prints it.
- `/model` without an argument (and the palette's "Switch model") opens a
  model picker: every catalog model grouped by provider in aligned columns
  (model, provider, alias, context, prices, capabilities, auth, status),
  the current model marked, unusable models dimmed with the reason, fuzzy
  filtering, keyboard and mouse navigation, and a refresh indicator while
  live lists update. It is 90 % of the terminal height and never wraps a
  row. `/model NAME` still switches directly.
- `harness --version` prints the git commit next to the version, e.g.
  `harness 0.1.0 (1a2b3c4d5e6f)`.
- The Anthropic provider gzips request bodies of 4 KiB or more when it talks
  to `https://api.anthropic.com` directly, which cuts upload time before
  the first token on slow links. If a server rejects the encoding, the raw
  body is resent once and gzip stays off for the rest of the process. Turn
  it off with `gzip_requests = off` in `[provider.anthropic]` or with
  `HARNESS_GZIP_REQUEST_BODIES=0`. The request inspector shows each call's
  raw and on-the-wire body size.
- Workflow steps placed with `runs_on` now run on their remote host. Agent,
  shell and verify steps push the worktree to the host, run their read,
  write, edit and bash tools there through the helper, and bring the
  changes back through the local policy and hygiene checks. Model calls and
  credentials stay local. Step and agent nodes record the host, and every
  remote operation and connection change is logged on the step tagged with
  its host. An agent step without its own `runs_on` uses its agent's.
  Placed steps and agent `bash` never send the local environment (the host's
  login environment is used), and every retry attempt gets a fresh remote
  run directory.
- Copy and paste in the TUI. Messages, code blocks, tool outputs and diffs
  have `⧉ copy` buttons; `ctrl+y` copies whatever has focus (else the last
  assistant message), the palette has "Copy last message", "Copy last code
  block", "Copy tool output", "Copy diff" and "Select text", node detail
  and the diff review copy with `y`, and `ctrl+t` starts a keyboard
  selection of transcript lines. Copies are plain text (no ANSI codes,
  borders or line numbers) with secrets masked, go out as OSC 52 (works
  over SSH and, with passthrough, tmux) or through wl-copy, xclip, xsel or
  pbcopy (`[ui] clipboard = osc52 | system | auto`), and a toast confirms
  each one. Bracketed pastes keep tabs, Unicode and line breaks exactly,
  never trigger keys, and also work in the palette filter, palette
  argument prompts and the memory search box.
- Per-host SSH compression: `[remote.<name>] compression = auto | yes |
  no`. `yes` adds `Compression=yes` to the host's ControlMaster, `no` adds
  `Compression=no`, and `auto` (the default) compresses only when the
  host's measured link throughput is below `compression_below_mbps`
  (default 20 Mbit/s). `/remote test` measures the link; a remote step
  measures it on connect when there is no measurement from the last 24
  hours. rsync sync uses `--compress-choice=zstd` when both rsyncs support
  zstd. `/remote` and its test action show whether compression is active.
- Remote helper protocol 2: file contents and process output travel as
  length-prefixed raw bytes instead of base64, and frames of 4 KiB or more
  are zstd-compressed (level 1) when both ends negotiate it in `hello`.
  Helper compression is skipped when SSH compression is active for the
  host. The frame limit applies to the decompressed size. Protocol 1
  helpers keep working, uncompressed. `cargo bench -p harness-remote
  --bench wire` reports bytes and latency at 10, 100 and 1000 Mbit/s.
- Content-hash cache for remote file transfers: the client and the helper
  each cache file contents by sha256 for the session (64 MiB per side), so
  re-reading an unchanged file, reading back a write, or writing content the
  host already has sends only the hash. Misses, evictions and mismatches
  fall back to a full transfer. `remote_op` events record `bytes_saved`;
  `harness remote-agent --content-cache BYTES` sizes the helper's cache.

## Defaults

Changes to built-in prompts, agents and workflows and to `harness.ini`
defaults. These change behaviour for most users, so every change needs an
entry here that says what changed and why, ending with the new defaults
fingerprint. CI fails until the entry exists; `cargo test -p harness-eval
--test defaults_changelog` prints the fingerprint to use. See
[docs/defaults.md](docs/defaults.md).

- Initial built-in `coder` (read, write, edit, bash, todo; read-write memory),
  `researcher` and `reviewer` agents and prompts, and the `dream` memory
  consolidation workflow. The coder prompt asks to verify before reporting
  done, stage files by name, keep changes minimal and remove temporary files.
  defaults-fingerprint: 00cfaf75a178
- The bundled workflows `implement-verify-review` (`/ivr`), `fix-bug`
  (`/fix`) and `plan-approve-implement` (`/plan`) are now built-in files
  (`crates/harness-config/assets/workflows/`), so the binary, `harness
  doctor` and evals all see them. Every built-in prompt gains a
  `context.md` layer telling the model that the harness manages the
  conversation's length and that it should keep going until the task is
  complete (it never says how much context is left), to prevent premature
  stops on long tasks.
  defaults-fingerprint: c0b3a2ab142f
- New `[ui] clipboard` setting, default `osc52`: copy actions use the OSC 52
  escape sequence, the only clipboard that works over SSH. New `[keys]`
  actions `copy` (default `ctrl+y`) and `select` (default `ctrl+t`).
  defaults-fingerprint: 9615b4d09acd
- New `[general] model_list_ttl` setting, default `24h`: how long cached
  live model lists (`<data_dir>/model-lists/`) stay fresh before the model
  catalog refetches them in the background.
  defaults-fingerprint: dbcc02b5b7e2
- New `[remote.<name>]` settings `compression` (default `auto`) and
  `compression_below_mbps` (default `20`): ssh connections to a host now
  always pass `Compression=yes` or `Compression=no`, so a `Compression`
  line in `~/.ssh/config` no longer applies to harness connections. An
  unmeasured or fast host stays uncompressed, as before; links slower than
  20 Mbit/s gain compression because it then saves more time than it costs.
  defaults-fingerprint: dbcc02b5b7e2
