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

- MCP client core (`crates/harness-mcp`). `[mcp.<name>]` sections in
  `harness.ini` define stdio servers (`command`, `args`, `env.*`, `cwd`)
  or remote streamable-HTTP servers (`url`, `header.*`), with `enabled`,
  `timeout`, `startup` and `startup_timeout`. `env.*` and `header.*`
  values are credentials, resolved like `api_key` (`_cmd`, `_env`) and
  redacted. The client speaks MCP 2026-07-28 and falls back to the
  `initialize` handshake for 2025-11-25 and older servers. Stdio servers
  start on first use, run in their own session and process group, restart
  with backoff after a crash, and are always cleaned up. They get the same
  environment scrubbing as the `bash` tool, and HTTP redirects are not
  followed, so credentials reach only the configured server. Their stderr
  goes to the log, redacted. `harness doctor` checks every server. See
  [docs/mcp.md](docs/mcp.md).
- OAuth for remote MCP servers (`auth = oauth` in `[mcp.<name>]`), following
  the MCP authorization spec: protected resource and authorization server
  discovery, pre-registered, client-metadata-document or dynamic client
  registration, authorization code with PKCE `S256`, `resource` and `iss`
  checks, and a loopback redirect. `harness login mcp <name>`, `/login mcp
  [name]` and the palette's *Authorize MCP server* entries run the browser
  flow; `harness logout mcp <name>` removes the tokens. Tokens live in
  `<data_dir>/auth/mcp/<name>.json` (mode `0600`, a protected path, never in
  harness.ini), are refreshed automatically and are redacted. Runs never open
  a browser and say how to authorize first. Bearer tokens travel only over
  TLS: `auth = oauth` with a plain `http://` url on a non-loopback host is a
  load error, and login, runs and metadata discovery refuse it too. New keys: `auth`,
  `oauth_client_id`, `oauth_client_metadata_url`, `oauth_scopes`. See
  [docs/mcp.md](docs/mcp.md#authorization-oauth).
- MCP tools for agents. Agent YAML selects them explicitly in `tools:`
  (`mcp:github/create_issue`, `mcp:db/*`) and workflow agent steps can add
  some with `tools:`; nothing from a server is offered otherwise, and only
  servers defined in harness.ini can be named. Every call is decided by the
  policy engine as `mcp:<server>/<tool>`: new `[policy] mcp.<server>.allow`,
  `.ask`, `.deny` and `.trust_annotations` keys, MCP tools are outward
  actions that ask in every mode by default, and session approvals,
  "always" rules and `harness run --allow 'mcp:<server>/*'` work for them.
  Results are labeled untrusted (`origin="mcp server"`), redacted,
  size-capped with spill pointers and recorded with new `mcp_call` events
  naming the server and tool. When the selected schemas exceed the new
  `[context] mcp_schema_budget` (default 2000 tokens), one `mcp_tools`
  search-and-call tool stands in for them. Tool-list change notifications
  (stdio, HTTP `GET` streams and modern `subscriptions/listen`) refresh the
  tools mid-session (`mcp_tools_changed` events) while the request prefix
  stays fixed until the next session. Tool cards, the map's live pane and
  node detail show MCP calls as `mcp <server> › <tool>` with duration and
  result. See [docs/mcp.md](docs/mcp.md#tools-for-agents).
- MCP in the TUI. `/mcp` lists every server with its status (connected,
  starting, failed with the reason, disabled, idle), transport, tool count
  and auth state, with buttons to restart a server, disable or enable it
  for the session, and view its redacted stderr; all clickable and
  keyboard-operable. Prompts of connected servers are slash commands and
  palette entries namespaced by server (`/github:review-pr 123`), with
  their arguments; a missing one is asked for in the palette. Resources
  can be attached to the next message from the palette (`Attach MCP
  resource: <server>/<name>`) and reach the model labeled as untrusted
  content (`origin="mcp server"`). The status line shows `⚠ mcp <name>`
  when a server has failed; it opens `/mcp`. See
  [docs/mcp.md](docs/mcp.md#in-the-tui).

### Fixed

- Failed tool cards (for example an MCP call whose approval was denied) no
  longer show the raw `<tool_result ...>` provenance wrapper around the
  error message.
- Tests no longer touch your real config or data. Some harness-eval runs
  started the `harness` binary without isolating its data directory, which
  left eval sessions (for example `/tmp/harness-eval-long-context-…`) in
  your sessions list. `harness eval run` now gives every attempt its own
  temporary data directory (`--shared-data-dir` opts back in to the
  configured one), the new `HARNESS_DATA_DIR` environment variable
  overrides `[general] data_dir` for one process, every test that runs the
  binary points `HARNESS_CONFIG`, `HOME` and the XDG directories at a temp
  dir, and `scripts/hermetic-guard.sh` (run in CI) fails if the harness or
  harness-eval suites write anything under `HOME` or the XDG directories.

## [0.2.0] - 2026-10-23

### Changed

- The chat transcript is bottom-anchored like a chat app: the newest
  message sits just above the input and a short conversation hugs the
  bottom of the pane. Scrolling up pauses following without moving the
  lines you read; a clickable, focusable `↓ new messages · end` button
  appears when output arrives, and End (or Ctrl+End) jumps back to the
  latest message. Resizing keeps the bottom anchored and the reading
  position stable.
- Runs and agents have human-readable names everywhere. Past runs in the
  palette read `Coder · fix the header parser · 14:32 Oct 23 · ✔` with the
  outcome (`Succeeded in 2m14s · $0.12 · 3 files changed`) as the
  description, instead of `Run agent coder` / `Open run <uuid>`. A run's
  title comes from its first prompt or its workflow inputs. The map
  header, node boxes, node detail, goal bar, subagent cards, `/runs`, the
  run history and `harness runs show` use the same names, and no raw UUID
  appears in the TUI: when two titles collide they get a short `#tag`,
  which `harness runs show` accepts next to full ids and id prefixes.
  Agent YAML gains an optional `display_name:` (default: the name with a
  capital first letter), shown in the agents' palette entries
  (`Switch to Coder: …`) and `/agents`; `run_started` records the run's
  title and agent display names. Narrow palettes, the run history and the
  map header cut the title rather than the tag, time, status or metrics.
- Sessions have short names, descriptions and last-activity times. A
  session is named from its first prompt (`Fix header parser`), or with
  the new `/rename` command, and its one-line description follows the
  conversation (first and latest request, files edited). `/resume` without
  an argument opens a fuzzy-searchable, clickable picker showing name,
  last message time (`14:32 today`, `yesterday 09:10`, `Oct 21 16:05`),
  project, model, message count, cost and description, most recently
  active first; the palette lists recent sessions the same way, and
  `harness sessions` prints one such line per session. `--resume` and
  `/resume` accept `last`, a name, an id prefix or a `#tag`. Names and
  descriptions are derived locally; the new `[general]
  session_summary_model` (off by default) lets a cheap model write them
  in the background after a turn, recorded as a `session-summary` node
  whose cost counts towards the session and budgets. Verifier feedback
  and compaction handoffs are not shown or counted as requests.

### Fixed

- The `/resume` picker and `harness sessions` shorten long project paths to their last
  directories (`…/work/proj`) and show the project last, so the time, message count, cost
  and model stay visible on deep paths such as macOS temp directories.
- `/new` (and other session restarts) no longer mixes the old run into the new one: the old
  session now finishes before the UI resets, so the palette, map and status show the new run
  (the palette's live run entry no longer stays at "Running for 0ms").
- Session names read better: a short first prompt stays whole (`hi there`
  is `Hi there`, not `There`), and a long one is cut on a word without a
  dangling preposition, article or conjunction (`Fix flaky login test`,
  not `Fix flaky login test in`). `/rename` confirms with `Session renamed
  to <name>`, and `harness sessions --help` (or `-h`) prints usage and
  exits 0 instead of listing sessions; other arguments are a usage error.
- The command palette's entry for the run in progress now shows its title as soon as the first prompt is known and how long it has been running, like the map and `/runs`, instead of an untitled "Running for 0ms" frozen at session start.
- From the second turn on, the activity line above the input no longer
  stays on "sending" (and later "no data for …") after the turn finishes.
  It now follows each turn's main agent node, so every turn's finish,
  including a cancelled or failed one, clears it, while subagent nodes
  still do not end it.
- A fresh interactive session shows its agent as ready instead of a
  spinning "running" in the footer and map header. A run counts as working
  only while a node runs, is queued or waits for approval, or while a
  workflow run has not created its first node yet.
- The palette and slash-completion frame-budget tests no longer fail on a
  single slow frame from a busy CI runner: each runs its key sequence five
  times on fresh state and checks the best run's slowest frame against the
  same 16 ms budget, so a genuinely slow implementation still fails.

## [0.1.0] - 2026-10-23

### Added

- Built-in identity prompt layer: every built-in agent's system prompt
  starts with a short layer that names harness and its features and points
  the model to the docs before it answers questions about them. Custom
  prompts can include it with `- builtin: identity`; `[general]
  identity_prompt = off` turns it off. `/context` lists it with its token
  count. A new eval suite (`crates/harness-eval/suites/identity`) and eval
  check `consulted:` verify that a feature question leads to reading the
  docs.
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

### Fixed

- Remote shell fallback: cancelling or timing out a command now kills its
  remote process group on hosts whose `/bin/sh` is dash (Debian, Ubuntu) or
  busybox ash (Alpine). Their builtin `kill` rejected the `--` the kill
  script used, so the remote command kept running.
- SSH on macOS: the ControlMaster socket directory no longer lives under
  the long per-user `$TMPDIR` (`/var/folders/…`), which made ssh fail with
  `ControlPath too long`. The socket directory is the first of
  `$XDG_RUNTIME_DIR/harness`, `$TMPDIR/harness-$USER` and
  `/tmp/harness-$USER` whose socket paths fit the platform's limit, and a
  symlinked socket directory is refused.
- bash tool: commands run in their own session with no controlling
  terminal. A command that opens `/dev/tty` (an ssh or sudo password
  prompt) now fails at once instead of being stopped in the background and
  hanging until the timeout, and a kill also continues stopped processes so
  they can exit on SIGTERM.
- Eval setup and `command` checks, and `api_key_cmd`, kill the command's
  whole process group on timeout, so background or compound commands no
  longer leave children running.
- Remote sync, the hygiene report's tracked-file list and memory's project
  key ignore an exported `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
  `GIT_OBJECT_DIRECTORY` or `GIT_COMMON_DIR` (set by git hooks), which
  pointed them at another repository. Test fixtures scrub `GIT_*` too.
- `scripts/build-remote-helpers.sh` works on macOS (`shasum` when there is
  no `sha256sum`). `scripts/verify-remote-helpers.sh` is Linux only and now
  says so when `readelf` or `timeout` is missing.

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
- Every built-in prompt (`coder`, `researcher`, `reviewer`) now starts with
  the built-in identity layer (`- builtin: identity`, about 115 tokens): it
  tells the model it runs inside harness, names the harness features
  (skills, workflows, agents, memory, the agent map, slash commands,
  harness.ini, remotes, ...) and asks it to read docs/guide/ and
  backlog/docs/ (locally, or at https://github.com/thowd22/harness) before
  answering questions about them, so it stops guessing. New `[general]
  identity_prompt` setting, default `on`; `off` removes the layer.
  defaults-fingerprint: 95e10042f5f1
- The built-in `coder`, `researcher` and `reviewer` agents set
  `display_name:` (`Coder`, `Researcher`, `Reviewer`), shown wherever an
  agent is named in the TUI and CLI. Prompts, tools and descriptions are
  unchanged, so model requests are unchanged.
  defaults-fingerprint: fb557d15ada9
- New `[general] session_summary_model` setting, default `off`: session
  names and descriptions are derived locally from the conversation, with
  no extra model call. Set a cheap model alias (for example `haiku`) to
  have it write them after each turn. Prompts, agents and model requests
  are unchanged.
  defaults-fingerprint: 00feb6d0d4e5
- New `[context] mcp_schema_budget` setting, default `2000` tokens: above
  it an agent's selected MCP tools are offered through the `mcp_tools`
  search-and-call tool instead of their full schemas. New `[policy]
  mcp.<server>.*` rules, empty by default, so every MCP tool asks before
  it runs. Built-in agents select no MCP tools, so prompts, agents and
  model requests are unchanged.
  defaults-fingerprint: 00feb6d0d4e5
