# MCP servers

harness is an MCP ([Model Context Protocol](https://modelcontextprotocol.io/specification))
client. It connects to MCP servers that you list in `harness.ini`. The
client lives in `crates/harness-mcp`.

Only `harness.ini` can add a server. Project YAML can refer to a server by
name (to select its tools, see [Tools for agents](#tools-for-agents)), but it
can never define one, because running a server means running a program or
sending credentials, and only you can allow that.

## Configuration

Each server gets its own `[mcp.<name>]` section. Names may use letters,
digits, `_` and `-`.

A local server runs as a subprocess and talks over stdio:

```ini
[mcp.github]
command = npx
args = -y, @modelcontextprotocol/server-github
env.GITHUB_PERSONAL_ACCESS_TOKEN_cmd = gh auth token
cwd = ~/src
```

A remote server is reached over streamable HTTP:

```ini
[mcp.docs]
url = https://mcp.example.com/mcp
header.Authorization_env = DOCS_MCP_TOKEN
timeout = 30s
```

| Key | Applies to | Default | Meaning |
|---|---|---|---|
| `command` | stdio | | Program to run, found on `PATH`. Set either `command` or `url`. |
| `args` | stdio | none | Arguments as a comma-separated list. An argument cannot contain a comma. |
| `cwd` | stdio | harness's directory | Working directory. A leading `~/` is expanded. |
| `env.<NAME>` | stdio | | An extra environment variable for the server (see [Credentials](#credentials)). |
| `url` | HTTP | | The MCP endpoint, `http://` or `https://`. |
| `header.<Name>` | HTTP | | An extra request header (see [Credentials](#credentials)). |
| `enabled` | both | `true` | `false` keeps the section but never starts the server. |
| `timeout` | both | `60s` | Time limit for each request. A request that takes longer is cancelled. |
| `startup` | both | `lazy` | `lazy` starts the server on first use. `eager` starts it when the session starts. |
| `startup_timeout` | both | `30s` | Time limit for starting the server and finishing the handshake. |
| `auth` | HTTP | `none` | `oauth` authorizes with OAuth (see [Authorization](#authorization-oauth)); `none` sends only `header.*`. |
| `oauth_client_id` | HTTP, `auth = oauth` | | A client id you registered with the server's authorization server. Used instead of dynamic registration. |
| `oauth_client_metadata_url` | HTTP, `auth = oauth` | | An `https://` URL of a Client ID Metadata Document listing `http://127.0.0.1/callback`. Used when the authorization server supports such documents. |
| `oauth_scopes` | HTTP, `auth = oauth` | what the server asks for | Scopes to request, comma-separated. |

Mistakes are load errors that give the line number: an unknown key,
`command` and `url` both set, `env.*` on a remote server, `header.*` or an
OAuth key on a stdio server, an `oauth_*` key without `auth = oauth`,
`auth = oauth` together with `header.Authorization`, `auth = oauth` with a
plain `http://` url on a host other than `127.0.0.1`, `localhost` or
`[::1]` (bearer tokens are sent only over TLS), or a zero timeout.

## Credentials

Every `env.*` and `header.*` value is treated as a credential, and is
resolved the same way as a provider `api_key`:

| Key | Source |
|---|---|
| `env.TOKEN = value` | the value written in the file (like `api_key`) |
| `env.TOKEN_cmd = pass show mcp/token` | the output of a command, run with `sh -c` with a 10 s limit (like `api_key_cmd`) |
| `env.TOKEN_env = MY_TOKEN` | the value of an environment variable (like `api_key_env`) |

`header.<Name>`, `header.<Name>_cmd` and `header.<Name>_env` work the same
way. If more than one source is set, the inline value wins, then `_cmd`,
then `_env`. A key ending in `_cmd` or `_env` always names a source, so an
environment variable that is really called `FOO_cmd` cannot be set.

These rules keep credentials safe:

- Credential values are never printed. `harness config show` prints
  `[redacted]` in their place, and error messages never include them.
- Each value is added to the server's redactor. Anything the server writes
  to stderr is masked before it reaches the log.
- A `harness.ini` that contains an inline `env.*` or `header.*` value must
  be private (`chmod 600`), just like one with an inline `api_key`.
- A stdio server does not inherit credential variables from harness's own
  environment. It gets the same scrubbing as the `bash` tool:
  `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`,
  `OPENROUTER_API_KEY`, every `HARNESS_*` variable, provider `api_key_env`
  variables, the variables named by `env.*_env` and `header.*_env`, and any
  variable whose value contains a resolved provider key are removed. Then
  the server's own `env.*` values are added, so it gets only the
  credentials configured for it.
- An HTTP server's redirects are not followed, so `header.*` credentials
  and requests reach only the configured `url`. A `3xx` response fails with
  the redirect target; set `url` to that final endpoint.

## Authorization (OAuth)

Many remote servers ask you to sign in instead of handing out a static
token. harness supports the OAuth flow from the MCP authorization spec:

```sh
harness login mcp notes        # or /login mcp notes in the TUI
```

```ini
[mcp.notes]
url = https://notes.example.com/mcp
auth = oauth                   ; set by harness login mcp
```

What `harness login mcp <name>` does:

1. It sends the server one request without credentials. The `401` answer
   names the server's protected resource metadata (RFC 9728); without one,
   harness tries the well-known URLs. The metadata must describe this
   server, and names its authorization server.
2. It reads the authorization server's metadata (RFC 8414 or OpenID Connect
   discovery, in the order the spec gives) and checks that the `issuer`
   matches. A server that does not offer PKCE `S256` is refused, and so are
   endpoints that are not `https://` (plain `http://` is allowed only on
   `127.0.0.1`, `localhost` and `[::1]`). The same rule covers the MCP
   server's own `url` and its protected resource metadata, even on the
   server's origin: harness never sends or obtains an access token over
   cleartext HTTP to a remote host.
3. It picks a client id: `oauth_client_id` if you set one, else
   `oauth_client_metadata_url` when the authorization server supports
   client metadata documents, else dynamic client registration (RFC 7591).
   A dynamic registration is kept and reused with the same authorization
   server only. If none of these work, the error says to register harness
   yourself and set `oauth_client_id`.
4. It opens your browser at the authorization page with PKCE (`S256`), a
   fresh `state`, the `resource` parameter (RFC 8707) and a redirect to
   `http://127.0.0.1:<port>/callback`. The terminal prints the URL too, in
   case the browser does not open. The answer's `state` and `iss` are
   checked before the code is used. Over SSH or without a display no
   browser is opened: open the link on another machine and paste the
   address it ends on back into harness, or forward the port with the
   `ssh -L` command harness shows (see [signing in from a remote or
   headless machine](providers.md#signing-in-from-a-remote-or-headless-machine)).
   Some authorization servers offer a simpler way for that case; see
   [the device flow](#signing-in-with-a-code-device-flow) below.
5. It exchanges the code for tokens, stores them and sets `auth = oauth` in
   the section.

### Signing in with a code (device flow)

When harness runs where the browser cannot reach it (an SSH session, no
display, or WSL) **and** the authorization server's metadata advertises a
`device_authorization_endpoint`, `harness login mcp <name>` and
`/login mcp <name>` use the device authorization grant (RFC 8628) instead
of the loopback redirect of step 4. Everywhere else (a desktop session, or a
server without that endpoint) nothing changes: the browser, paste-back and
`ssh -L` flow above runs as before. The MCP authorization spec does not
require the device grant, so most servers will not offer it.

1. harness asks the authorization server for a code (with the client id,
   the scopes and the `resource` parameter). A dynamic registration asks for
   the device grant as well; an earlier registration without it is replaced
   once. PKCE does not apply, since no authorization code is involved.
2. It shows a link and a short code, for example:

   ```text
   Not opening a browser here (this is an SSH session). To authorize MCP server notes, open this link in a browser on any device:
   https://auth.example.com/device?user_code=WDJB-MJHT
   and check that it shows this code:
   WDJB-MJHT
   (Or open https://auth.example.com/device and type the code.)
   The code expires in 15 minutes. Nothing needs to be pasted back here.
   ```

   Open the link on any device (your laptop, a phone), sign in and approve.
   The link and the code are each on a line of their own, so the terminal's
   selection copies them; the link is an OSC 8 hyperlink where the terminal
   supports it, and typing `c` (copy the link) or `u` (copy the code) and
   Enter copies with OSC 52. In the TUI the sign-in dialog shows the link,
   the code and the plain page, with `[ copy link ]` (ctrl+y),
   `[ copy code ]` (`c`) and `[ cancel ]`.
3. Meanwhile harness polls the token endpoint every `interval` seconds the
   server names (5 if it names none), 5 seconds slower after each
   `slow_down`. When you approve, it stores the tokens like step 5. A
   declined request (`access_denied`) or an expired code (`expired_token`,
   or the code's `expires_in` running out) ends the sign-in with a message
   and saves nothing; so does the overall five-minute limit. Ctrl+C, or Esc
   or `[ cancel ]` in the TUI, stops polling at once.

There is nothing to paste in this flow. If the server refuses the device
grant for harness's client (for example `unauthorized_client`), harness
says nothing and continues with the loopback redirect, so paste-back and
`ssh -L` still work. The device code, the user code and the tokens are
never logged or put in events.

Scopes: harness asks for the scopes in the server's `401` challenge (else
its `scopes_supported`), plus scopes it asked for earlier, plus
`offline_access` when the authorization server offers it (for a refresh
token). `oauth_scopes` replaces this choice. If a server later answers
`403 insufficient_scope`, the call fails with the missing scope and the
next `harness login mcp` asks for it as well.

Tokens are personal credentials and get the same protection as the Sign in
with ChatGPT tokens:

- They live in `<data_dir>/auth/mcp/<name>.json`, mode `0600`, in `0700`
  directories, never in `harness.ini`. The `auth` directory is a built-in
  protected path, so agent tools can never read it, and it is never synced
  to remote hosts.
- They are tied to the server's `url`: if you change `url`, harness stops
  sending the old token and asks you to authorize again.
- They are sent only in the `Authorization: Bearer` header, never in a URL,
  and redirects are never followed. Stored tokens are added to the
  redactor, and they are never logged or put in events.
- harness refreshes the access token shortly before it expires, and once
  after the server rejects it with `401`, then retries the request. The
  refresh token is rotated when the server issues a new one. Refreshes are
  serialized, so parallel agents never use the same refresh token twice.
- If the refresh token has expired or was revoked, harness deletes the
  stored tokens (it keeps the client registration) and the error says to
  run `harness login mcp <name>` again.

**Runs never open a browser.** A TUI turn, `harness run`, a workflow or an
eval that needs a server without valid tokens fails with "needs OAuth
authorization ...; run `harness login mcp <name>` (in the TUI: /login mcp
<name>) first". Authorize once (over SSH too, see step 4) before running
headless jobs.

`harness logout mcp <name>` deletes the stored tokens and registration
(after confirmation; `--yes` skips it).

In the TUI, `/login mcp` opens a panel that lists the remote servers and
whether they are authorized, with an **authorize** button for each (click
it, or Tab to it and press Enter). `/login mcp <name>` and the palette
entries **Authorize MCP server: <name>** start the flow straight away. The
flow runs in the background. A sign-in dialog shows the link in full
(`[ copy link ]` or ctrl+y copies it), the *Address* field for pasting the
address the browser ends on, the `ssh -L` command over SSH
(`[ copy ssh command ]`), and then the result; a toast reports the result
too. For a [device flow](#signing-in-with-a-code-device-flow) it shows the
link and the code instead, with `[ copy code ]`. Esc or `[ cancel ]` stops
the flow; a click outside the dialog does not.

Static tokens keep working: with `auth = none` (the default), set
`header.Authorization` (inline, `_cmd` or `_env`) as before. A server that
answers `401` with an OAuth challenge while `auth = none` fails with a
message that says to set `auth = oauth` and run `harness login mcp <name>`.

Not supported: confidential clients (a pre-registered client secret) and
the client credentials grant.

## Tools for agents

Nothing from an MCP server reaches a model unless an agent selects it.
List MCP tools in the agent's `tools:` as `mcp:<server>/<tool>`; `*` in
the tool part is a wildcard:

```yaml
name: triager
prompt: coder
tools: [read, mcp:github/create_issue, mcp:github/list_*, mcp:db/*]
```

`<server>` must be a `[mcp.<server>]` section in harness.ini; an agent or
workflow that names another server fails to load ("names MCP server
`ghost`, which harness.ini does not define"). A project agent that shadows
one of yours may not add MCP tools to it, like any other tool.

A workflow agent step can add MCP tools for that step with `tools:`. Only
`mcp:` entries are allowed there; built-in tools come from the agent:

```yaml
steps:
  - id: file-issue
    agent: implementer
    tools: [mcp:github/create_issue]
    prompt: "File an issue for {{ steps.repro.outputs.test_name }}"
```

The model sees each selected tool as `mcp__<server>__<tool>` (characters
other than letters, digits, `_` and `-` become `_`, and long names are cut
to 64 characters with a hash), with the server's description and argument
schema. When the selected tools are too large for the context budget, they
are offered through one `mcp_tools` tool instead (see
[Context cost](#context-cost)).

Servers an agent selects are started and listed when the session (or the
agent) starts; agents that select nothing start nothing.

## Permissions

Every MCP call goes through the policy engine as `mcp:<server>/<tool>`,
like `bash` commands do. Decisions show in the approval prompt, the
`policy_decision` event and the tool card. In order:

1. `[policy] mcp.<server>.deny`: the call never runs.
2. `[policy] mcp.<server>.ask`: the call needs approval.
3. `[policy] mcp.<server>.allow`: the call runs without asking.
4. With `mcp.<server>.trust_annotations = true`, a tool that the server
   marks `readOnlyHint: true` runs without asking (and may run in parallel
   with other read-only calls). Annotations are untrusted by default, as
   the MCP spec requires.
5. Otherwise the tool may act outside your workspace (open issues, write a
   database, send a message), so it is an **outward action**: it asks in
   every autonomy mode, including `auto`.

```ini
[policy]
mcp.github.allow = get_*, list_*, search_*
mcp.github.ask = create_*, update_*
mcp.github.deny = delete_*
mcp.docs.trust_annotations = true
```

The values are lists of tool names with `*` wildcards (the server's own
tool names, not the `mcp__` names). Only harness.ini can loosen MCP
policy; agent and workflow YAML cannot.

Approving "this session" in the prompt allows that tool (the suggested
pattern is `mcp:<server>/<tool>`) until the session ends; it never
overrides a deny. "Always" also proposes `mcp.<server>.allow = <tool>` for
harness.ini, written only after you confirm. Headless runs deny calls that
need approval; with `--approvals allowlist`, `--allow 'mcp:github/*'`
allows MCP tools for the run.

## Context cost

Large servers expose dozens of tools, and sending every schema with every
request would fill the context. harness estimates the selected tools'
definitions (4 bytes per token, like the initial-context check) and
compares them with `[context] mcp_schema_budget` (default `2000` tokens):

- Within the budget, each tool is offered under its own name.
- Over it, one `mcp_tools` tool (about 150 tokens) stands in for all of
  them. The model calls it with `search` (a few words; an empty search
  lists every tool) to get matching tools with their argument schemas,
  then with `call` (`server/tool`) and `arguments`. Arguments are checked
  against the tool's schema before the server sees them, and the error
  says how to fix them.

Either way the definitions are fixed when the session starts, so the
request prefix stays the same for the whole session and the provider's
prompt cache keeps working. The CI initial-context check runs with a
150-tool server configured (`crates/harness-mcp/tests/initial_context.rs`).

## Results

- Results are labeled untrusted for the model, whatever the project's
  trust: `<tool_result tool="mcp__github__get_issue" ... trust="untrusted"
  origin="mcp server">`. This holds for the top agent, its subagents and
  workflow agent steps alike. The system prompt tells the model not to
  follow instructions inside such markers.
- They are redacted with the server's own credentials (`env.*`,
  `header.*`, OAuth tokens) and every other credential harness knows.
- Large results are cut to the `bash` output budget (16 000 bytes): the
  model gets the head and tail and a `spill:<id>` pointer that `read` can
  page through.
- Text, embedded text resources and `structuredContent` (when there is no
  text) are passed on; the first image goes to models that accept images;
  other media and resource links are described in one line.
- A tool that reports an error (`isError: true`) is a failed call with the
  server's message; protocol errors and timeouts say what failed.
- Every call is recorded: the `tool_call` (the name the model used), an
  `mcp_call` event with the server and the server's tool name, the
  `policy_decision`, and the `tool_result` with duration and output (see
  [events.md](events.md)).

In the TUI, the chat's tool cards, the agent map's live pane and node
detail show MCP calls as `mcp <server> › <tool>`, with the arguments sent,
the duration and the result. Cards expand and collapse with a click, or
with Tab and Enter like every card.

## Tool-list changes

Servers that declare `tools.listChanged` tell harness when their tools
change: legacy servers send `notifications/tools/list_changed` on stdio or
on the HTTP `GET` stream; modern servers send it on a
`subscriptions/listen` stream that harness opens for them (stdio and
HTTP). harness re-reads the list at once, without restarting the session
or the server, and records an `mcp_tools_changed` event (shown as a notice
in the chat) listing the selected tools that appeared or disappeared.

- `mcp_tools` searches and calls use the new list immediately.
- A tool that disappeared fails with "no longer offers" instead of
  reaching the server.
- Tools offered under their own names keep the definitions they had at
  session start, so the request prefix only changes at session boundaries;
  new tools appear under their own names in the next session.

A server that restarts after a crash is re-read too, since its tools may
differ.

## In the TUI

### The /mcp panel

`/mcp` (also in the palette) lists every `[mcp.<name>]` server:

```text
SERVER         STATUS                         VIA    TOOLS  AUTH
bad            failed (1x, retries on use)    stdio      -  no credentials
  reason: could not start `mcp-bad`: No such file or directory (os error 2); check `command` and `cwd` in [mcp.bad]
docs           connected (MCP 2026-07-28)     http       8  oauth: authorized
  prompts: /docs:summarize · resources: 3
github         idle (starts on first use)     stdio      -  env credentials
off            disabled (harness.ini)         stdio      -  no credentials
```

- **Status** is `connected` (with the protocol version), `starting`,
  `failed` with the reason on the next line (`retries on use`, or `gave
  up` after 5 failures in a row), `disabled (harness.ini)`, `disabled
  (this session)`, or `idle` for a lazy server that has not started yet.
- **Tools** is the number of tools a connected server offers.
- **Auth** never shows a credential: `oauth: authorized`, `oauth: not
  authorized (/login mcp <name>)`, `header credentials`, `env credentials`
  or `no credentials`.

Each enabled server has three buttons: `restart <name>` (stop it, forget
its failures, so a server that gave up gets another chance, and start it
again), `disable <name>` (stop it for the rest of the session; its tools
then fail with a message saying how to enable it again) or `enable <name>`,
and `logs <name>`. A server disabled in `harness.ini` only has `logs`.
`logs` shows the status, the last error and the last stderr lines with
credentials masked; remote servers have no stderr, so their requests and
errors are only in the harness log. `back` returns to the list. The text
updates by itself while the panel is open, for example when a server
finishes starting.

Buttons are clickable, Tab / Shift+Tab or Left/Right move between them,
Enter activates one, and Esc closes the panel.

When a server has failed, the status line shows a red `⚠ mcp <name>` (or
`⚠ mcp <n> failed`). Click it, or reach it with Shift+Tab from the message
box and press Enter, to open `/mcp`.

### Prompts

A connected server's prompts become slash commands named
`/<server>:<prompt>`, with the prompt's arguments, and palette entries
(kind `mcp`). Arguments are given in the server's order; the last one takes
the rest of the line, so `/github:review-pr 123 focus on the tests` passes
`focus on the tests` as the second argument. `name=value` works too. If a
required argument is missing, the palette asks for the arguments.

Prompts of a lazy server are listed once it is running (after a tool call,
the restart button, or `startup = eager`). You can also type
`/<server>:<prompt>` before that: harness starts the server and looks the
prompt up. A built-in, workflow, skill or agent command with the same name
wins.

The prompt's text becomes your next message. Assistant messages in it are
marked `(assistant)`, images and audio are left out with a note, and an
embedded resource is wrapped as untrusted content (see below).

Running a prompt, reading a resource, and the `/mcp` restart, disable and
enable buttons talk to the server in the background, so a slow or starting
server never holds up the TUI: interrupting (Ctrl+C) still stops a running turn,
and quitting does not wait for the server. A toast says when the work is
done (or why it failed).

### Resources

A connected server's resources are palette entries named
`Attach MCP resource: <server>/<name>`, described by title, URI and type,
so searching for the server or the name finds them. Choosing one reads it
and attaches it to your next message (a toast confirms; `/mcp` lists what
is attached and has a `detach resources` button). Binary content is left
out with a note, and text is cut at 100 000 characters.

Attached content is always untrusted. It is redacted (with the server's
own `env.*` and `header.*` credentials as well as every credential harness
knows, like the server's tool results) and wrapped like this before the
model sees it, and text inside it cannot close the marker:

```text
<web_content url="test://readme" server="docs" trust="untrusted" origin="mcp server">
...
</web_content>
```

## Lifecycle

- **Lazy start.** By default a server starts on first use. `startup = eager`
  starts it in the background when the session starts.
- **Own session and process group.** A stdio server runs in its own session
  (`setsid`). It has no controlling terminal, so a password prompt fails
  instead of hanging, and harness can signal its whole process group.
- **Shutdown.** harness closes the server's stdin and waits 2 s. If the
  server is still running, harness sends SIGTERM to its process group,
  waits another 2 s, then sends SIGKILL. Processes the server started in
  its group are killed too. If a server handle is dropped without a
  shutdown, its group is killed at once, so no orphans are left behind.
  When a server exits on its own, the rest of its group is killed right
  away. After that harness never signals that process group id again,
  because the system may have reused it for an unrelated process.
- **Crashes.** If a stdio server exits unexpectedly, the requests in flight
  fail with `exited (...)` and the last lines the server wrote to stderr.
  They are not retried, because a tool call may already have had side
  effects. The next request restarts the server after a backoff that
  starts at 0.5 s and doubles each time, up to 30 s. A successful request
  resets the backoff. After 5 failures in a row, harness stops restarting
  the server for the rest of the session.
- **Timeouts.** A request that runs longer than `timeout` fails and is
  cancelled: over stdio harness sends `notifications/cancelled`, and over
  HTTP it closes the response stream. A server that hangs on one request
  can still answer the others.

## Protocol versions

harness works with servers from both generations of the protocol:

- **Modern** (`2026-07-28`). Every request carries the protocol version and
  the client capabilities in `_meta`. On HTTP, every request also carries
  the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers, plus
  `Mcp-Param-*` headers for tool arguments marked with `x-mcp-header`. A
  remote tool whose `x-mcp-header` annotations are invalid is dropped from
  the tool list, and the log says why.
- **Legacy** (`2025-11-25`, `2025-06-18`, `2025-03-26`). These versions use
  the `initialize` handshake, which negotiates the version and
  capabilities, followed by `notifications/initialized`. On HTTP the
  session uses the `Mcp-Session-Id` header and ends with `DELETE`. If the
  session expires, harness sets up a new one.

harness finds out which generation a server speaks the way the spec
describes. Over stdio it first sends `server/discover`. If the server gives
no modern answer within 10 s (or `startup_timeout`, if that is shorter),
harness falls back to `initialize`. Over HTTP it sends a modern
`server/discover` first, and falls back when it gets back a `4xx` that is
not a modern MCP error. The older HTTP+SSE transport (`2024-11-05`) is not
supported.

harness offers no client features (roots, sampling or elicitation). If a
tool needs more input from the client, the call fails with "the server
asked for more input".

## Logs

A server's stderr never appears in the terminal. Each line goes to the
harness log under the target `harness_mcp::stderr`, with the server name,
after credentials have been masked (see [logging.md](logging.md)). The
last 20 lines are also kept for error messages.

harness skips stdout lines that are not valid JSON-RPC, logs a warning for
each one, and keeps the connection open. A response that has neither
`result` nor `error` fails only the request it answers.

## harness doctor

`harness doctor` checks every enabled server. It starts the server,
finishes the handshake, lists the tools and then stops the server again:

```text
[ok  ] mcp github: github-mcp-server 1.2.0; MCP 2026-07-28 (modern); 31 tool(s); ready in 412 ms
[FAIL] mcp docs: MCP server `docs`: HTTP 401 Unauthorized for `server/discover`: unauthorized (check the header.* credentials in the server's [mcp.<name>] section)
       fix: check the header.* credentials (for example header.Authorization_env) in [mcp.docs]
[skip] mcp old: disabled (enabled = false)
```

## Troubleshooting

| Symptom | What to do |
|---|---|
| `could not start ...` | The command is not installed or not on `PATH`, or `cwd` does not exist. Run the command by hand. |
| `did not finish starting within ...` | The server is slow to start (for example, `npx` downloading a package) or it hangs. Raise `startup_timeout`, or run the server by hand. |
| `exited (exit code N); stderr: ...` | The server crashed. The stderr excerpt usually says why. The full output is in the log under `harness_mcp::stderr`. |
| `failed N times in a row` | The server keeps crashing. Fix it, then start a new session. |
| `HTTP 401` / `403` | A `header.*` credential is wrong or missing. Check its `_cmd` or `_env` source. |
| `needs OAuth authorization` | Run `harness login mcp <name>` (or `/login mcp <name>` in the TUI). |
| `the server asks for OAuth authorization` | Set `auth = oauth` (or just run `harness login mcp <name>`, which sets it). |
| `url ... is not https://; OAuth bearer tokens are sent only over TLS` | Change `url` to the server's `https://` endpoint. |
| `the OAuth authorization expired or was revoked` | Run `harness login mcp <name>` again. |
| `needs more OAuth permissions` | Run `harness login mcp <name>`; it asks for the missing scope too. |
| `supports neither dynamic client registration nor ...` | Register harness with the server's provider (redirect URI `http://127.0.0.1/callback`) and set `oauth_client_id`. |
| `did not answer ... within ...` | The tool is slow. Raise `timeout`. |
| `supports protocol versions ...` | The server and harness have no protocol version in common. Update one of them. |
| `names MCP server ..., which harness.ini does not define` | Add a `[mcp.x]` section, or fix the name in the agent's or step's `tools:`. |
| `... matches no tool of MCP server ...` | The server has no such tool; run `harness doctor` to see its tool count, or use a `*` pattern. |
| An MCP call always asks, even in `auto` | MCP tools are outward actions by default. Allow them with `[policy] mcp.<server>.allow`, or approve them for the session. |
| `no longer offers` | The server removed the tool during the session; search again with `mcp_tools`. |
| The model sees `mcp_tools` instead of the tools | The selected schemas exceed `[context] mcp_schema_budget`; raise it or select fewer tools. |

## For developers

- Agent tools: `harness_mcp::tools::McpToolbox` (implements
  `harness_agent::subagents::ExtraTools`) turns selections into agent
  tools; `ToolboxOptions` holds the schema budget, trusted annotations and
  the spiller. Policy: `harness_policy::mcp` (`McpRules`,
  `ReadOnlyTools`, `action_name`). With `--agent-tools` the test server
  also offers `write_note`, `fail`, `big_output`, `structured`, `add_tool`
  and `remove_tool` (which announce list changes), and `--bulk-tools N`
  adds N filler tools for context-cost tests.
- Library: `harness_mcp::{Servers, ServerHandle, ClientOptions, Connection}`.
  `ServerHandle` has `start`, `ping`, `list_tools`, `call_tool`, `request`,
  `status` and `shutdown`. `harness_mcp::kill_all()` kills every stdio
  server's process group, for exit paths that skip destructors.
- OAuth: `harness_mcp::auth` (`Authorize`, `OAuthSession`,
  `McpTokenStore`, `discovery`). `harness_mcp::auth::mock` has a local
  mock of an OAuth-protected MCP server and its authorization server, and a
  `MockBrowser`, for offline tests.
- Test server: `harness_mcp::test_server` is a minimal MCP server for tests.
  It speaks modern, legacy or both, over stdio (the
  `harness-mcp-test-server` binary; `test_server::binary_path()` finds or
  builds it from another crate's tests) and over HTTP
  (`test_server::HttpServer`, in-process, with JSON or SSE responses). Its
  tools exercise echo, progress notifications, slow calls, crashes,
  malformed output, stderr, environment checks, grandchild processes,
  `x-mcp-header` and input requests.
- End-to-end test: `crates/harness/tests/mcp_e2e.rs` runs `harness run`
  with an agent that selects test-server tools, over stdio and streamable
  HTTP in both protocol generations. It checks that the tools are listed
  and offered, that the calls are policy-checked (one allowed, one asking
  and denied headless), that the run log records `tool_call`, `mcp_call`,
  `policy_decision` and `tool_result`, and that the run log, folded into
  the chat view, shows `mcp t › <tool>` cards that expand by click and by
  Enter.
