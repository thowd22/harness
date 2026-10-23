---
id: doc-2
title: harness.ini Reference
type: specification
created_date: '2026-10-22 14:26'
updated_date: '2026-10-23 08:07'
---
# harness.ini Reference

Single user-owned config file. Location: `$HARNESS_CONFIG`, else `$XDG_CONFIG_HOME/harness/harness.ini` (default `~/.config/harness/harness.ini`).

Rules:
- File must be mode `0600` (owner read/write only) because it contains credentials; harness refuses to load secrets from a group/world-readable file (`harness doctor` explains the fix).
- The file is a **protected path**: agent tools can neither read nor write it.
- Unknown keys are errors with line numbers (catches typos, a classic fragility source).
- Every run records a hash of the effective (redacted) config.
- Secrets may be given inline (`api_key`), via command (`api_key_cmd`), or via environment variable name (`api_key_env`). First non-empty wins in that order.
- `harness login <provider>` checks a key with one cheap authenticated request and then writes `api_key`, `api_key_cmd` or `api_key_env` into `[provider.<id>]`, removing the other two from that section, keeping every other line and comment, and setting the file to `0600`. `harness logout <provider>` removes all three after confirmation. Anthropic is API key only (Claude subscription login is not supported under Anthropic's terms); OpenAI takes an API key or Sign in with ChatGPT; OpenRouter takes an API key. See docs/providers.md#logging-in.
- Sign in with ChatGPT (`harness login openai --chatgpt`, or the fourth choice of `harness login openai` and `/login openai`) never writes tokens to harness.ini: OAuth tokens rotate, so they live in the token store `<data_dir>/auth/chatgpt-auth.json` (mode `0600`, directory `0700`), which is a built-in protected path. Login only sets `auth = chatgpt` in `[provider.openai]`; storing an API key with `harness login` removes `auth` again. `harness logout openai` removes `auth` with the key lines and deletes the stored ChatGPT tokens (the client registration and `ext_agent_host_id` in `chatgpt-host.json` are kept for the next sign-in). See docs/providers-openai.md#sign-in-with-chatgpt.
- Lists are comma-separated. Booleans: `true/false`.

## Example

```ini
[general]
default_model = sonnet           ; alias or provider/model
mode = auto-edit                 ; ask | auto-edit | auto
data_dir = ~/.local/share/harness
max_subagent_depth = 2
max_concurrent_agents = 4
model_list_ttl = 24h             ; cached live model lists stay fresh this long
identity_prompt = on             ; on | off: built-in identity prompt layer
todo_nudge_after = 3             ; model calls with no todo list before a reminder; 0 = off
session_summary_model = off      ; off | a cheap model alias: names and describes sessions

[provider.anthropic]
api_key = sk-ant-...
; api_key_cmd = pass show anthropic
base_url = https://api.anthropic.com
gzip_requests = auto                ; gzip large request bodies (auto | off)
gzip_level = 3                      ; 1 (fastest) to 9 (smallest)

[provider.openai]
api_key_env = OPENAI_API_KEY
; auth = chatgpt                ; api_key (default) | chatgpt (Sign in with ChatGPT)
; chatgpt_headless = false       ; allow auth = chatgpt in harness run / wf / eval (never in CI)

[provider.openrouter]
api_key = sk-or-...
app_name = harness
provider_order = anthropic,openai   ; OpenRouter routing preference
allow_fallbacks = true

[models]
; aliases used by agents/workflows and /model
opus   = anthropic/claude-opus-5-5
sonnet = anthropic/claude-sonnet-5-5
haiku  = anthropic/claude-haiku-5-5
gpt    = openai/<model-id>
router = openrouter/<vendor>/<model-id>

[pricing]
; optional overrides, USD per million tokens: input,output,cache_read,cache_write
anthropic/claude-sonnet-5-5 = 3,15,0.3,3.75

[keys]
palette   = ctrl+p
send      = enter
newline   = shift+enter, alt+enter
cancel    = esc
interrupt = ctrl+c
switch_view = tab
quit      = ctrl+q
copy      = ctrl+y                ; copy the focused message, code block, output or diff
select    = ctrl+t                ; keyboard text selection in the transcript

[ui]
theme = dracula                  ; dracula | none (more themes later)
background = theme               ; theme (paint Dracula bg) | terminal
animations = true                ; spinners; false = static indicators
stall_after = 20                 ; seconds without data before the indicator turns orange
timestamps = time                ; time | relative | off
tool_preview_lines = 8
terminal_title = true            ; spinner + status in the terminal title
show_thinking = collapsed        ; hidden | collapsed | expanded
markdown = true
mouse = true                     ; click any visible element; false restores native text selection
clipboard = osc52                ; osc52 (works over SSH/tmux) | system (wl-copy, xclip, xsel, pbcopy) | auto
goal_bar = auto                  ; auto (shown only when tasks exist) | off
goal_bar_width = 36
live_preview = true              ; one-line live output/reasoning preview on map nodes

[theme]
; optional per-token overrides of the active theme (hex)
; accent = #bd93f9
; running = #8be9fd

[policy]
; glob patterns over parsed simple commands; deny > ask > allow
bash.allow = cargo build*, cargo test*, git status*, git diff*, ls*, rg*
bash.ask   = git commit*, npm install*
bash.deny  = git push --force*, rm -rf /*
paths.protected = ~/.ssh/**, **/.env*, ~/.aws/**
outward.ask = git push*, gh pr create*, cargo publish*, npm publish*
; MCP tools, by server: tool-name wildcards; deny > ask > allow
mcp.github.allow = get_*, list_*
mcp.github.ask   = create_*
mcp.github.deny  = delete_*
mcp.github.trust_annotations = false ; true: readOnlyHint tools run without asking

[sandbox]
enabled = true                   ; landlock (fs) on Linux
network = allow                  ; allow | deny (bwrap/unshare when available)

[verify]
command = cargo test --workspace
baseline = true
max_fix_attempts = 3

[tools]
; limits for read/write/edit/bash and output spill (see docs/tools.md)
read_max_lines = 2000
read_max_bytes = 32000
read_max_line_chars = 2000
read_max_file_bytes = 67108864
bash_timeout = 120               ; seconds, default per call
bash_max_timeout = 600           ; seconds, cap for a call's timeout
spill_over_bytes = 16000         ; larger outputs go to a blob + head/tail
spill_head_lines = 40
spill_tail_lines = 40

[context]
compact_at = 0.8                 ; fraction of model window
keep_recent_turns = 6
elide_tool_results_over = 4000   ; tokens
mcp_schema_budget = 2000         ; tokens of selected MCP tool schemas before mcp_tools stands in

[budget]
session_usd = 10
run_usd = 5
daily_usd = 30
on_exceed = ask                  ; ask | stop

[memory]
enabled = true
drive = ~/.local/share/harness/memory
dream = daily                    ; off | daily | manual
dream_model = haiku
index_max_tokens = 1500
headless_access = false          ; automations/evals don't read/write memory

[remote.gpu-box]
host = 10.0.0.12
user = me
port = 22
identity_file = ~/.ssh/id_ed25519
labels = gpu, bigmem
workdir = ~/harness-work
max_jobs = 2
sync = rsync                     ; rsync | git
compression = auto               ; auto | yes | no (ssh channel compression)
compression_below_mbps = 20      ; auto: compress links slower than this
policy.bash.deny = sudo*

[mcp.github]                     ; local MCP server over stdio
command = npx
args = -y, @modelcontextprotocol/server-github
env.GITHUB_PERSONAL_ACCESS_TOKEN_cmd = gh auth token
startup = lazy                   ; lazy | eager

[mcp.docs]                       ; remote MCP server over streamable HTTP
url = https://mcp.example.com/mcp
header.Authorization_env = DOCS_MCP_TOKEN
timeout = 30s

[mcp.notes]                      ; remote MCP server with OAuth (harness login mcp notes)
url = https://notes.example.com/mcp
auth = oauth                     ; none | oauth
```

## Section reference (summary)

| Section | Purpose |
|---|---|
| `general` | default model, autonomy mode, data dir, agent limits, model list cache TTL |
| `provider.<id>` | `anthropic`, `openai`, `openrouter`: credentials, base_url, provider-specific options |
| `models` | aliases → `provider/model` refs |
| `pricing` | per-model price overrides |
| `keys` | keybindings for the handful of global actions |
| `ui` | theme, animations, timestamps, tool previews, mouse, goal bar |
| `theme` | per-token color overrides for the active theme |
| `policy` | bash allow/ask/deny, protected paths, outward-action guard, MCP tool rules |
| `sandbox` | landlock/network sandbox |
| `verify` | default verification command + gating |
| `tools` | read caps, bash default/max timeout, output spill threshold |
| `context` | compaction thresholds and elision |
| `budget` | spend caps |
| `memory` | memory drive and dreaming |
| `remote.<name>` | SSH hosts available to `runs_on` |
| `mcp.<name>` | MCP servers (stdio or streamable HTTP); the only place a server can be defined |
## Value syntax and validation (as implemented)

- Inline comments start with `;` preceded by whitespace; full-line comments start with `;` or `#`. `#` inside a value is literal (`accent = #bd93f9`).
- Keys may contain `.` `_` `-` `/` `:` `+` `@` (so `[pricing]` keys can be model refs). Duplicate sections or keys are errors.
- Durations (`stall_after`, `model_list_ttl`): bare number = seconds, or suffix `ms`, `s`, `m`, `h`.
- Paths (`data_dir`, `memory.drive`, `identity_file`) expand a leading `~/`; `remote.*.workdir` is a remote path and is not expanded locally.
- `[pricing]` values take 2 to 4 numbers: `input,output[,cache_read[,cache_write]]`.
- `[keys]` accepts only the actions `palette, send, newline, cancel, interrupt, switch_view, quit`. `[models]` values must be `provider/model`. `[theme]` values must be `#rrggbb`, and keys must be token names: `bg, surface, fg, muted, reasoning, running, path, link, ok, warn, approval, stalled, user, keyword, accent, agent, error, number, highlight, border, border.focus, card.pending, card.ok, card.error, card.approval, diff.added, diff.removed, diff.added.bg, diff.removed.bg` (see docs/tui.md). `[keys]` chords are `mod+key` (`ctrl`, `alt`, `shift`, `super`); a chord bound to two actions, or to the fixed suspend key `ctrl+z`, is a load error.
- `provider.<id>` accepts `anthropic`, `openai`, `openrouter`. Every provider takes `api_key`, `api_key_cmd`, `api_key_env` and `base_url`. Only openrouter also takes `app_name`, `provider_order` and `allow_fallbacks`. Only openai also takes `auth` (`api_key`, the default, or `chatgpt`: Sign in with ChatGPT, using the user's ChatGPT plan; requests then never use the API key, with no fallback) and `chatgpt_headless` (boolean, default `false`; headless runs, workflows and evals refuse `auth = chatgpt` unless it is `true`, and refuse it whenever `CI` is set, because the credential is personal). Only anthropic also takes `gzip_requests` (`auto`, the default, or `off`) and `gzip_level` (1 to 9, default 3). With `auto`, request bodies of 4 KiB or more are gzipped only when `base_url` is `https://api.anthropic.com` on the default port and no proxy (`HTTPS_PROXY`, `ALL_PROXY`) or custom CA (`SSL_CERT_FILE`, `SSL_CERT_DIR`) is set. The environment variable `HARNESS_GZIP_REQUEST_BODIES=0` also turns it off. See docs/providers-anthropic.md.
- `remote.<name>` requires `host`. Defaults: `port = 22`, `max_jobs = 1`, `sync = rsync`, `compression = auto`, `compression_below_mbps = 20`. A remote can only tighten policy, with `policy.bash.deny` and `policy.bash.ask`.
- `remote.<name> compression` sets SSH channel compression for that host: `yes` adds `Compression=yes` to the host's ControlMaster, `no` adds `Compression=no`, and `auto` (the default) compresses only when the host's last measured link throughput is below `compression_below_mbps` (a positive number of Mbit/s). The link is measured by `/remote test` and, when no measurement younger than 24 hours exists, when a remote step connects; an unmeasured host is not compressed. Compressed connections use their own ControlMaster socket (`cmz-%C`). rsync sync uses `--compress-choice=zstd` when both rsyncs support zstd, and rsync's default compression otherwise. See docs/remote.md.
- `mcp.<name>` (name: letters, digits, `_`, `-`) defines one MCP server and requires exactly one of `command` (stdio: with `args`, a comma-separated list, `cwd`, `~` expanded, and `env.<NAME>`) or `url` (`http://` or `https://`, streamable HTTP, with `header.<Name>`). Every server also takes `enabled` (default `true`), `timeout` (per request, default `60s`), `startup` (`lazy`, the default: start on first use, or `eager`: start with the session) and `startup_timeout` (start plus handshake, default `30s`); both timeouts must be greater than 0. `env.*` on a remote server, `header.*`, `args` or `cwd` on the wrong kind of server, and unknown keys are load errors. Every `env.<NAME>` / `header.<Name>` value is a credential with the provider-key sources: inline `env.NAME`, `env.NAME_cmd` (a command, like `api_key_cmd`) and `env.NAME_env` (an environment variable, like `api_key_env`), first non-empty wins in that order; a key ending in `_cmd` / `_env` always names a source. Values never print (`config show` shows `[redacted]`), are added to the server's redactor, and an inline value makes the file subject to the `0600` rule like an inline `api_key`. Stdio servers get the bash tool's environment scrubbing (default provider key names, `HARNESS_*`, provider `api_key_env` variables, variables named by `env.*_env` / `header.*_env`, and values containing a resolved provider key) before their own `env.*` is added. HTTP redirects are not followed, so `header.*` credentials reach only `url`. A remote server also takes `auth` (`none`, the default, or `oauth`: OAuth per the MCP authorization spec, with tokens from `harness login mcp <name>` or the TUI `/login mcp <name>` kept in `<data_dir>/auth/mcp/<name>.json`, mode `0600`, inside the protected `auth/` directory, never in harness.ini, refreshed automatically; runs never open a browser and fail with a message naming the login command) and, with `auth = oauth` only, `oauth_client_id` (a pre-registered client, used instead of dynamic registration), `oauth_client_metadata_url` (an `https://` Client ID Metadata Document URL, used when the authorization server supports them) and `oauth_scopes` (comma-separated scopes instead of the ones the server asks for). `auth = oauth` together with `header.Authorization`, `auth = oauth` with a plain `http://` url on a non-loopback host (only `127.0.0.1`, `localhost` and `[::1]` may use `http://`; bearer tokens travel only over TLS, per RFC 6750 section 5.3 and OAuth 2.1 section 1.5, and login, runs and discovery refuse it as well), or any of these keys on a stdio server, is a load error. `harness login mcp <name>` sets `auth = oauth` itself. Project YAML can reference a server by name but never define one. See docs/mcp.md.
- `[policy] mcp.<server>.allow`, `mcp.<server>.ask` and `mcp.<server>.deny` are lists of the server's tool names with `*` wildcards; `mcp.<server>.trust_annotations` is a boolean (default `false`). An MCP call is decided as `mcp:<server>/<tool>`: deny, then ask, then allow, then (with trusted annotations) `readOnlyHint: true` tools run without asking; any other MCP tool is an outward action that asks in every mode, including `auto`. Session approvals and "always" (`mcp.<server>.allow = <tool>`, written after confirmation) apply as for commands. Only harness.ini loosens MCP policy. A key under `mcp.` that is not `mcp.<server>.allow|ask|deny|trust_annotations` is a load error. See docs/mcp.md.
- `[context] mcp_schema_budget` (tokens, default `2000`): when the definitions of an agent's selected MCP tools are estimated above it, the agent gets the single `mcp_tools` search-and-call tool instead. `0` always uses `mcp_tools`.
- `api_key_cmd` runs under `sh -c` with a 10 s timeout, stdin closed and stderr discarded. It leads its own process group, and a timeout kills the whole group.
- A file that holds an inline `api_key` and is group- or world-readable is refused with a `chmod 600` hint.
- Defaults when a key is absent: `default_model = sonnet`, `mode = auto-edit`, `data_dir = $XDG_DATA_HOME/harness` (`~/.local/share/harness`; the `HARNESS_DATA_DIR` environment variable overrides `data_dir` for one process: `harness eval run` sets it to a fresh directory per attempt unless `--shared-data-dir`, and tests use it to stay off the user's data, see docs/data-dir.md), `max_subagent_depth = 2`, `max_concurrent_agents = 4`, `model_list_ttl = 24h` (live model lists for the model catalog are cached in `<data_dir>/model-lists/` and refetched in the background when older; see docs/models.md), `identity_prompt = on` (every built-in prompt starts with the built-in identity layer, at most 120 tokens, which names harness and its features and points the model to docs/guide/, backlog/docs/ and https://github.com/thowd22/harness before it answers questions about them; `off` removes it from every prompt, including custom prompts that use `- builtin: identity`; also accepts `true`/`false`; see docs/defaults.md), `session_summary_model = off` (session names and descriptions are derived locally from the conversation with no model call; set a cheap model alias such as `haiku` to have it write the description, and the name unless the user chose one with `/rename`, after every turn; see docs/sessions.md). `todo_nudge_after = 3` (when an agent that has the `todo` tool has made this many model calls in one turn and the run's task list is empty, its next request carries a short reminder to plan with `todo`; it repeats at most every N calls and stops once a task exists; `0` turns it off; see docs/subagents.md). `[ui]` defaults are the values in the example above. `[policy]` lists are empty (the engine adds built-ins). `sandbox.enabled = true`, `network = allow`. `verify.command` is auto-detected, `baseline = true`, `max_fix_attempts = 3`. `[context]` defaults are as in the example. Budgets have no cap and `on_exceed = ask`. Memory defaults are as in the example, with `drive = <data_dir>/memory`.
- `harness config check [path]` validates the file and exits 1 on any error.
