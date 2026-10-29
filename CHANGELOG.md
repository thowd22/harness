# Changelog

All notable changes to harness are listed here.

New entries go under `## Unreleased`. Cutting a release (the *Cut release*
workflow, `scripts/release-bump.sh`) renames that section to
`## [X.Y.Z] - YYYY-MM-DD`, and its text becomes the GitHub release notes
(`scripts/release-notes.sh`). See [docs/releasing.md](docs/releasing.md).
The `## Defaults` section at the end is separate: it records every change
to built-in prompts, agents, workflows and `harness.ini` defaults.

## Unreleased

## [0.7.0] - 2026-10-29

### Added

- Every reasoning effort level on every provider: `/effort` and agent YAML
  `effort` take `minimal`, `low`, `medium`, `high`, `xhigh`, `max` and
  `ultra`. Each model knows the levels it accepts, from the live model
  list where it says (Anthropic's `capabilities.effort`, the ChatGPT
  plan's `supported_reasoning_levels`, for example `low` to `max`), else
  from its provider and family (Claude 4.7+ `low` to `max`, GPT-5
  `minimal` to `high`, GPT-5.2+ `low` to `xhigh`, OpenRouter `minimal` to
  `xhigh`). A level the model does not accept is sent as the nearest one it
  does, for the main agent and subagents, and `/effort` says so
  (`effort ultra: sent as max`). The `/effort` panel offers the levels the
  current model takes. Claude budget models get xhigh (32,768) and max
  (49,152) thinking budgets.

- `yolo`, a permission mode above `auto` that never asks: whatever would
  ask in `auto` (`bash.ask` and MCP rules, the outward-action guard for
  pushes and publishes, the staging guard, edits outside an agent's scope)
  runs without a prompt, for the main agent and subagents. Deny rules,
  protected paths and the sandbox still apply. Set it with `[general] mode`,
  `harness run --mode yolo`, `/mode yolo`, the palette or Shift+Tab; it
  warns when chosen. Agent YAML cannot set it.
- Shift+Tab in the chat input steps through the permission modes (ask →
  auto-edit → auto → yolo → ask), like Claude Code. Rebind it with
  `[keys] cycle_mode`.

### Changed

- The status line shows the permission mode first, left of the working
  directory, and keeps it when the terminal is too narrow for anything
  else; `yolo` shows in the error colour.
- Alt+Up (`[keys] focus_transcript`) now moves focus from the chat input to
  the cards, buttons and indicators above it, since Shift+Tab there changes
  the mode. Once something is focused, Shift+Tab and the arrows move focus
  as before.
- Cached live model lists from 0.6.0 and earlier are fetched again on the
  next refresh, so they pick up effort levels; until then the cached list
  is still used.

## [0.6.0] - 2026-10-28

### Added

- Sign in with ChatGPT shows the models your plan actually includes: harness
  fetches the signed-in account's model list from OpenAI and lists it in
  `/model` and `harness models` in OpenAI's order with its display names
  (for example GPT-6-Astra, GPT-5.6-Sol). It is cached per account and
  fetched again after each sign-in. Before it has been fetched, the GPT-5
  and later and Codex families count as plan models.
- Models that only a live model list knows (new Claude models from
  Anthropic's list, new OpenRouter models, the ChatGPT plan models) now run
  with that list's context window, output limit, thinking, image support
  and price instead of the conservative 32k defaults.

### Changed

- OpenAI models named `gpt-6` and later are treated like `gpt-5`: they get
  reasoning effort and the GPT-5 limits when nothing better is known.

## [0.5.1] - 2026-10-27

### Fixed

- A request with the wrong `state` sent to the sign-in's loopback port (a
  stale browser tab, or someone else's request) no longer ends Sign in with
  ChatGPT or MCP OAuth. It gets a "stale link" page, and the sign-in keeps
  waiting for the real callback, as it already did for a wrong paste.
- The `/login` and `harness login` method hints name the chosen provider's
  own command and variable (`pass show openai`, `OPENAI_API_KEY`) instead
  of always showing the Anthropic examples.

## [0.5.0] - 2026-10-27

### Added

- Browser sign-in works when the browser is on another machine (SSH,
  no display, WSL): Sign in with ChatGPT and MCP OAuth no longer try to open
  a local browser there. They show the sign-in link in full (an OSC 8
  hyperlink; copy it with `[ copy link ]`/ctrl+y in the TUI or `c` in
  `harness login`) and take the address the browser ends on, pasted back
  into harness, with the same `state`, `iss` and PKCE checks as a real
  callback. Over SSH they also show the exact `ssh -L` port-forward command.
  In the TUI both sign-ins run in the background in a new sign-in dialog
  (keyboard and mouse), and Esc cancels them and closes the loopback
  listener. See
  [docs/providers.md](docs/providers.md#signing-in-from-a-remote-or-headless-machine).
  No default changed.
- MCP OAuth signs in with a code (the device authorization grant, RFC 8628)
  when harness runs over SSH, without a display or in WSL and the server's
  authorization server advertises a `device_authorization_endpoint`:
  `harness login mcp <name>` and `/login mcp <name>` show a link and a short
  code to approve on any device (copy them with `c`/`u` in the CLI,
  `[ copy link ]`/`[ copy code ]` in the TUI) and poll until you approve,
  honouring `interval`, `slow_down`, `access_denied` and expiry; Esc
  cancels. Other servers and desktop sessions keep the browser, paste-back
  and `ssh -L` flow; a server that refuses the device grant falls back to
  it. See [docs/mcp.md](docs/mcp.md#signing-in-with-a-code-device-flow).
  No default changed.
- harness tells you when a new release is out: when the TUI starts it
  checks the latest GitHub release in the background (one plain GET, at
  most once a day, silent when offline or rate-limited) and, if it is
  newer than the running build, shows a dialog with both versions, the
  release-notes link and the `curl … install.sh | bash` command, with a
  Copy button (keyboard and mouse; OSC 52, so it works over SSH). It waits
  until the chat input is empty and no turn is running, so it never takes
  keys meant for a prompt. Esc or `[ close ]` closes it, and a dismissed
  version is not shown again until a newer one ships.
  `[general] update_check = off` turns it off; `harness run`, `wf run`,
  `bench` and `eval` never check. See [docs/update-check.md](docs/update-check.md).

## [0.4.0] - 2026-10-24

### Added

- `/effort [default|low|medium|high]` sets the reasoning effort of the
  session's agent without copying its YAML: the next request carries it
  (Anthropic adaptive `output_config.effort`, a thinking budget on Claude
  Haiku 4.5, `reasoning.effort` on OpenRouter and OpenAI), the status line
  shows it after the model, and the session file keeps it for resume.
  `default` restores the agent YAML's effort or the model default; a bare
  `/effort` opens a picker with the active effort and where it comes from.
  On a model that does not reason it says so and nothing is sent. See
  [docs/commands.md](docs/commands.md#effort). No default changed.
- Slash completion offers a command's argument choices after `/name `
  (`/effort l` → `low`, `/mode a` → `ask`, `auto-edit`, `auto`), by
  keyboard and mouse.
- `[context] background_compaction` (off by default, with
  `background_compact_at = 0.7`): the handoff summary starts in the
  background at 0.7 of the window and is swapped in at `compact_at`, so the
  turn that crosses it no longer waits for the summary call; the latest
  `keep_recent_turns` turns and every turn added meanwhile stay verbatim,
  and a failed, cancelled or outdated summary falls back to in-line
  compaction. `harness bench --fake --compaction` measures it (summary
  wait 2.5 s to 0.06 ms in the bundled run). See
  [docs/context.md](docs/context.md#background-compaction-experimental-off-by-default).
  The fake provider gains `match_system` for scripted replies matched by
  system prompt.
- `harness bench`: a latency benchmark of a bundled two-turn coding task
  (fix a bug, add a method with a test) in a temporary git repository.
  `--fake` runs offline against the scripted fake provider and measures
  harness overhead; `--model REF` runs a real model. It reports, per model
  call, time to first byte and token, output tokens/s, call time, cached and
  uncached input, request sizes and cache-prefix stability, and per gap the
  tool, verify, hook and harness-overhead time, plus turn and tool totals,
  event-recording and TUI render cost, as a table and JSON. See
  [docs/performance.md](docs/performance.md), which also records the
  baseline. The timing probe exists only under `harness bench`. CI runs
  `scripts/bench-guard.sh` on Ubuntu and macOS: `harness bench --fake`
  best of 7, failing when harness overhead per call or session start
  passes its limit (see
  [docs/performance.md](docs/performance.md#ci-overhead-guard)).
- OpenAI requests carry a `prompt_cache_key` (a hash of the instructions
  and tools, shared by every turn and by subagents with the same prefix),
  so cache-warm turns route to the same prompt cache. New opt-in
  `[provider.openai] prompt_cache_retention = 24h` (default `default`:
  nothing is sent, so the organisation's own retention applies, including
  in-memory caching under Zero Data Retention) asks for 24-hour retention
  on the models that support it. See
  [docs/providers-openai.md](docs/providers-openai.md).
- `[experimental]` section in harness.ini for opt-in speed experiments, all
  off by default: `prewarm_connection` (open the provider connection while
  the session starts).
- `[experimental] early_tool_start` (off by default): a reply's leading
  read-only tool calls that policy allows without asking start as soon as
  each call is complete in the stream, instead of after the whole message;
  results, approvals and the event log are unchanged, and stream errors,
  retries and cancellation cancel them. Every provider now reports each tool
  call's end as soon as its arguments are complete: OpenRouter closes a call
  when the next one or text starts and at `finish_reason` (before, only at
  the end of the message). See
  [docs/performance.md](docs/performance.md#early-tool-start).
- Anthropic and OpenRouter request options and cache experiments, all off
  by default (TASK-161.4): `[provider.anthropic] thinking_display =
  summarized | omitted | updates` (`updates` adds its beta header),
  `cache_ttl = 5m | 1h | mixed`, and `cache_ttl_auto_1h` (one-hour
  breakpoints while a background job runs or after a long tool round);
  `[provider.openrouter] reasoning_exclude`; `[experimental] cache_prewarm`
  (a non-streaming `max_tokens: 0` call writes the prompt cache when an
  interactive session starts or resumes, after `/compact` and before a
  subagent fan-out, recorded as a `cache-prewarm` node with its cost) and
  `stagger_subagents` (sibling requests with the same cached prefix wait
  for the first response to begin, then read the cache). See
  [docs/providers-anthropic.md](docs/providers-anthropic.md) and
  [docs/performance.md](docs/performance.md#prompt-cache-options).
- `[provider.openrouter] service_tier = default | fast` (unset by default:
  nothing is sent). `fast` asks OpenRouter for the priority tier, which for
  Claude Opus 5.5 is Anthropic fast mode at 2x the token price (live:
  turns 2.2x faster, session cost +73%, eval pass rate unchanged).
  OpenRouter model ids with the routing suffixes `:nitro` and `:floor`
  (e.g. `openrouter/anthropic/claude-opus-5.5:nitro`) are now accepted:
  the catalog check uses the base model's entry (before, they were
  rejected as unknown models). See
  [docs/performance.md](docs/performance.md#fast-mode-nitro-reasoning-and-effort-task-1617).
- Select text with the mouse anywhere in the TUI and copy it on release.
  Press, drag and release over the chat transcript (messages, tool cards
  and their output), the input box, the map, node detail, the diff screen,
  panels and pickers: the selection is highlighted (new `selection` theme
  token) and copied through `[ui] clipboard` with a toast. The copy is the
  logical text: soft-wrapped lines are rejoined and borders, gutters,
  icons and button labels are left out. Dragging past the edge of a
  scrollable view scrolls it; double-click selects a word, triple-click a
  line; Esc or a click clears it and `ctrl+y` copies it again. A press
  without movement is still a click; clicks on a card's body and on
  `/model` and `/resume` picker rows act on the release, so a drag over
  them selects text instead of toggling, picking or resuming. Text under a
  toast is still copied. The `select` key
  (`ctrl+t`) and "Select text" also work on the map and other screens: a
  cursor moved with the arrows, extended with Shift, copied with Enter; in
  the chat, Shift+Up/Down extends the line selection.
- Built-in `grep` and `glob` tools, so agents search without bash. `grep`
  is a ripgrep-style regex search (honours `.gitignore`, skips `.git`,
  binary and huge files) with `files_with_matches`, `content` (`-n`, `-A`,
  `-B`, `-C`) and `count` modes, `glob`/`type` filters, `-i`, `multiline`,
  `head_limit` and `offset`; `glob` lists files by pattern, newest first.
  Both are read-only (they run in parallel with other reads), are
  policy-checked like `read`, never show protected paths, go through the
  output budget and run on remote hosts through the helper (`fs.find`).
  The `coder`, `researcher` and `reviewer` agents have them. Before, a real
  session on a large repo ran 82 of 131 tool calls through bash, mostly
  `cd <repo> && grep -rn ...` over `vendor/`, one tool per model call.
- Background bash jobs. `bash` takes `run_in_background: true`: the call
  returns at once with a job id and a log file, the command keeps running in
  its own process group (locally, in the sandbox or on a remote host through
  the helper or the shell fallback), and when it exits harness appends a
  short notice (command, exit status, run time, last output lines, log path)
  to the agent's next request, so the model no longer polls with long
  `sleep`s. The new `bash_jobs` tool (offered with `bash`) lists jobs, reads
  a job's new output and stops a job (its whole process group). Jobs belong
  to the session: the top-level agent, its subagents and workflow agent
  steps (also on remote hosts) share them, each agent is told about the
  jobs it started (the top-level agent also about jobs of subagents that
  have ended), and they stop when the session ends (quit, `/new`,
  `/resume`) or, for a workflow's jobs, when the workflow run ends. At most
  4 run at once (`[tools] bash_max_jobs`) and each log keeps 32 MiB
  (`[tools] bash_job_output_bytes`). The TUI shows `⚙ N jobs` in the status
  line (click or focus + Enter opens `/jobs`); `/jobs` lists jobs with
  their elapsed time and has buttons to open a job's output or stop it.
  New `bash_job` event. (TASK-153)

### Changed

- Faster session start and warm connections (TASK-161.1). The OpenRouter
  tool-support check at session start reads the cached model list
  (`<data_dir>/model-lists/`, fresh for `[general] model_list_ttl`) instead
  of downloading the ~800 KB catalog every time (session start 0.3-0.8 s
  before, 6.2 ms with a cached list); a stale list is refreshed in the background, and models the
  list lacks or marks tool-less are still checked against the live
  catalog. All providers for the same base URL (main agent, subagents,
  session summaries, dreaming, the catalog download) now share one HTTP
  connection pool with longer keepalive (idle 300 s, TCP and HTTP/2
  keepalive), so later providers skip the cold DNS/TCP/TLS setup; each
  provider still sends only its own credentials. OpenRouter requests carry
  a `session_id` by default (`[provider.openrouter] session_id`).

- The TUI fits every view on screen and inside its border at any terminal
  size, and no longer cuts text off with `…`: long words, paths, URLs,
  code and diff lines and CJK/emoji wrap inside tool cards, subagent
  cards, notices, panels, pickers, popups and toasts; card headers keep
  the tool name in the border and wrap the rest inside the box. The status
  line, footer, activity line and selection bar wrap onto more rows; when
  the status line still runs out of room, a `+N more` button (and the new
  palette entry **Show status**) opens the full status in a panel. Popups
  size to their content and scroll when they cannot fit. Tabs are drawn as
  spaces to 4-column stops. A layout test matrix checks every view at
  40x12 to 250x60. See [docs/tui.md](docs/tui.md#layout-everything-fits-nothing-is-hidden).
  (TASK-160)

- `bash_jobs` has a `wait` action: it blocks until a background job ends
  (or `timeout` seconds, default 60, at most 600) and returns its exit
  status and new output, so agents stop polling with `sleep`. The exit is
  then not repeated as a notice.

- The coder now keeps a todo list by default, so the goal bar fills in.
  The coder prompt and the `todo` tool description tell the model to add
  each step before starting any task with several steps. When an agent with
  the `todo` tool has made `[general] todo_nudge_after` model calls in a turn
  (default 3) and the task list is still empty, harness adds a short
  reminder to its next request, repeated at most that often and dropped once
  a task exists. The reminder goes on the newest, not-yet-sent tool-result
  message, so earlier requests and their prompt cache are unchanged, and it
  is recorded in the run log. `todo_nudge_after = 0` turns it off.
- The activity line no longer reports a long, quiet tool as `no data`. A
  running tool shows its elapsed time against its timeout
  (`running bash: cargo test 4m12s of 10m`) and, for `bash`, the CPU time
  of its process group, sampled every 3 s (Linux `/proc`, macOS `ps`; also
  through the remote helper as `exec.liveness`) and recorded as new
  `tool_progress` events. Only a sampled tool with no output, no CPU
  progress and nothing runnable for `[ui] stall_after` gets a soft
  `bash quiet for 59s (no output, no CPU) · ctrl+c to cancel` hint.
  Provider silence still warns, now worded
  `⚠ waiting on the model: no data for 41s`.
- The `memory` tool states its limits up front and takes several bullets
  per write. Its description and schema say each bullet is one line of at
  most 400 characters (`maxLength` on `text`), and the new `bullets` array
  (up to 10) saves several lessons for one note in one commit, each checked
  on its own; rejected ones are listed by number with the reason. A bullet
  that is too long is refused with its length, the limit and how to split
  it. Asked to "index this codebase into memory", the agent now writes one
  compact `project/overview` note (at most 10 bullets of up to 1000
  characters) plus non-obvious gotchas instead of a file map, and tells
  you the rest is re-read from the repo. Before, a real session had 15 of
  37 memory writes rejected as too long and rewrote them 2-3 times.

### Fixed

- A background job still running when a session or workflow run ended
  could miss its `bash_job` `stopped` event: the run log could close
  between the job's exit and the event. Stopping, waiting for and shutting
  down jobs now wait until the end is recorded.
- OpenRouter: a model whose catalog entry lacks the `reasoning` parameter
  no longer gets a `reasoning` object when an effort is set.
- TUI: `/model NAME` typed while the agent works ran as a prompt after the
  turn (the command text went to the model); queued slash commands now
  run as commands.
- Running a test suite or a build no longer ends a turn with
  "unverified: N file changes" when the only changes are generated
  artifacts. Bash changes to well-known build and test output
  (`__pycache__/`, `*.pyc`, `.pytest_cache/`, `.mypy_cache/`, `target/`,
  `node_modules/`, coverage files, `.DS_Store`, ...) are recorded as
  generated (`file_changed` gains `"generated": true`): they stay visible in
  node detail and `/diff` (listed last, labelled `generated`) but do not
  count toward the unverified notice, `verify_when = on_changes` or the
  node's files-changed count, and checkpoints leave them out. A workspace
  below its repository root now also honours the parent `.gitignore` files
  up to that root (never a `.gitignore` above the repository, and not
  when that repository ignores the workspace itself, as a dotfiles home
  whose `.gitignore` is `*` does). Source files changed by bash, tracked
  files that match the list (a source directory named `target/`), and
  tracked files an ignore rule matches (force-added), still count. See
  docs/hygiene.md.
- Copy-on-select polish: copying a tool card's header no longer keeps a
  piece of its border rule when the title is truncated (`line-th…─ 7ms`
  is now `line-th… 7ms`), and a drag that starts on a panel, picker or
  dialog border selects from the nearest text cell inside it, as one from
  a chat card's border already did.
- Prompt caching works for Claude models on OpenRouter. Requests to
  `anthropic/...` models now carry up to four `cache_control` breakpoints,
  placed like the Anthropic provider's (last tool, system prompt layers,
  the latest conversation turns); before, every call was billed as
  uncached input. Other OpenRouter models are unchanged (they cache
  automatically). New `[provider.openrouter] cache_ttl = 5m | 1h`. Cache
  writes are now recorded on `model_call_finished` usage (`cache_write`)
  and shown in the status line, node detail and `/cost`.
- Terminal ioctls work in the bash sandbox. The static (musl) release build
  lost the Landlock rule for `/dev/tty` (Rust's std drops `O_PATH` on musl,
  so the launcher really opened `/dev/tty`, which fails without a
  controlling terminal), and `ioctl(TIOCGWINSZ)` / `tcgetattr` on
  `/dev/tty` then failed with `Permission denied`, breaking terminal-size
  probes and pty-based TUI tests. The launcher now opens rule paths with
  `O_PATH` itself and grants device ioctls only on the terminals
  (`/dev/tty`, `/dev/ptmx`, `/dev/pts` and the command's own terminal);
  ioctls on other devices stay denied on Landlock ABI 5+ kernels.
- A sandboxed command that fails with `Permission denied` or
  `Operation not permitted` outside the writable paths now ends its output
  with a one-line `harness sandbox:` note naming `[sandbox]` in
  harness.ini, so the model stops guessing at the cause.
- The end-of-session memory extractor and the dream pass accept
  `project/overview` bullets up to 1000 characters, like the memory tool;
  before, they applied the 400-character limit to every path and dropped
  valid overview proposals. The per-path limit now has one definition
  (`guard::limit_for_path`) shared by all three.

## [0.3.0] - 2026-10-23

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
- The coder prompt and the `todo` tool description now ask the model to add
  each step to `todo` before starting any task with several steps (models
  skipped the softer wording and the goal bar stayed empty), and the new
  `[general] todo_nudge_after = 3` reminds agents that still have no task
  list after three model calls.
  defaults-fingerprint: 8328cb67af31
- Focus and efficiency defaults after a real session on a large repo ran 82
  of 131 tool calls through bash, one tool per model call, polled long
  commands with `sleep` and had 15 of 37 memory writes rejected:
  - The built-in `coder`, `researcher` and `reviewer` agents get the new
    read-only `grep` and `glob` tools, and their prompts a short shared
    `tools.md` layer: search with `grep`/`glob` and read with `read` (bash
    for builds, tests and git), make independent tool calls in the same
    turn, and the bash working directory persists (no `cd <dir> &&`
    prefix). The researcher prompt no longer suggests `ls`/`grep` through
    bash. (TASK-152)
  - The `bash` tool description and schema gain `run_in_background`, and
    the built-in agents that have `bash` are also offered the new
    `bash_jobs` tool, so long builds, tests and servers run as background
    jobs whose exit is reported in a later message. New `[tools]
    bash_max_jobs = 4` and `bash_job_output_bytes = 33554432` (32 MiB).
    (TASK-153)
  - The memory prompt layer (`memory.md`, used by the `coder`) and the
    read-write `memory` tool description state the bullet limits (one
    line, at most 400 characters; 1000 in `project/overview`), ask for
    several lessons for one note in one write with the new `bullets`
    argument, and answer "index this codebase into memory" with one compact
    `project/overview` note plus non-obvious gotchas instead of a file map.
    (TASK-155)
  - New `[provider.openrouter] cache_ttl`, default `5m`: requests to
    `anthropic/...` models on OpenRouter carry `cache_control` breakpoints;
    other models' requests are unchanged. (TASK-151)
  - After a live Opus 5.5 session: `bash_jobs` gains `wait` (block until a
    job ends, default 60 s, at most 600 s) and the tools layer asks for it
    instead of `sleep`; the tools layer and the `edit` description say to
    open files with `read`, not `cat`, because `edit` needs a `read` first
    (the model's first edit failed after it had used `cat`).
  defaults-fingerprint: 46b4cecb7b8e
- Speed work (TASK-161). One default changes; every other new setting is
  opt-in and off by default, and request bodies with the defaults are
  byte-identical except for OpenRouter's `session_id` and OpenAI's
  `prompt_cache_key` (golden snapshots reviewed):
  - New `[provider.openrouter] session_id`, default on: every OpenRouter
    request carries one `session_id` per harness process, so the session
    (subagents, compaction and summaries included) keeps routing to one
    upstream and its warm prompt cache, also after pauses. Measured with
    no change in cost, cache share or latency; `session_id = false` turns
    it off. It replaces the pre-release `[experimental]
    openrouter_session_id`, which is now an unknown key. (TASK-161.1)
  - New `[experimental]` section, all `false`: `prewarm_connection`
    (TASK-161.1), `early_tool_start` (TASK-161.2), `cache_prewarm` and
    `stagger_subagents` (TASK-161.4).
  - `[provider.openai] prompt_cache_retention`, default `default` (nothing
    sent; `24h` opt-in). `prompt_cache_key` is sent on API-key requests;
    it is derived from the instructions and tools, so prompts are
    unchanged. (TASK-161.3)
  - `[provider.anthropic] thinking_display` (default `summarized`;
    `omitted` or `updates`), `cache_ttl` (default `5m`; `1h` or `mixed`:
    1h on the tool and system breakpoints, 5m on the messages) and
    `cache_ttl_auto_1h` (default `false`); `[provider.openrouter]
    reasoning_exclude` (default `false`). (TASK-161.4)
  - `[context] background_compaction` (default `false`) and
    `background_compact_at` (default `0.7`). (TASK-161.5)
  - `[provider.openrouter] service_tier` (unset: nothing sent; `fast` or
    `default`). The coder's effort stays unset: low effort was 24% faster
    and 40% cheaper with every eval passing, but the suites pass on every
    arm, so a harder eval comes first. (TASK-161.7)
  Prompts and tool descriptions are unchanged.
  defaults-fingerprint: 46b4cecb7b8e
- New `[general] update_check` setting, default `on`, and
  `update_check_interval`, default `24h`: the interactive TUI asks GitHub
  for the latest harness release in the background at startup, at most
  once per interval, and offers a newer one in a dialog with the install
  command. Headless commands never check. Prompts are unchanged. (TASK-164)
  defaults-fingerprint: d38458c654e8
