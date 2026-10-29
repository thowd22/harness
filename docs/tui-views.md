# Chat views

This page covers what you see and use in the chat screen: the transcript,
tool cards, the activity line, the input, the command palette, approvals,
notifications and the status line. The event loop, colours and key bindings
are covered in [tui.md](tui.md).

All colours come from theme tokens, so `[theme]` overrides and `NO_COLOR`
apply everywhere.

## Layout

```
transcript ...                                         toasts (top right)
⠹ thinking 3.1s · turn 4 · 12.3s · ↑12.4k (cache 11.9k) ↓812 · $0.021 · ctrl+c to interrupt
╭ ⏸ approve bash  $ git push origin main ──────────────────── outward action ╮
› your message
auto-edit │ ~/CODE/harness  main │ sonnet │ ctx ▓▓▓░░░░░ 41% │ ↑182k ↓14k cache 93% │ $0.84 │ verify ✔ │ ◐ 3 agents │ ⧉ gpu-box
```

From the top: the transcript, the activity line (only while the agent
works), pending approvals, the input, and the status line.

## Transcript

- Your messages have a pink bar. Each assistant turn starts with
  `◆ assistant` and ends with a muted metadata line:
  `sonnet · ↑12.4k (cache 96%) ↓812 · reasoning 1.2k · $0.021 · 4.2s · end_turn`
  (model, input tokens with the cache hit rate, output and reasoning tokens,
  cost, duration and stop reason). Cost shows `$?` when it is unknown.
- Assistant text is rendered as Markdown while it streams: headings, lists,
  quotes, `code`, **bold**, *italic*, links, rules and fenced code blocks.
  Code blocks are syntax highlighted (Rust, Python, JavaScript/TypeScript,
  Go, shell, C/C++, Java/Kotlin, JSON/TOML/YAML). `[ui] markdown = false`
  shows the raw text.
- Reasoning follows `[ui] show_thinking`: `hidden` shows nothing,
  `collapsed` shows a `▸ thinking · N lines` line you can expand, and
  `expanded` shows it in italics.
- Errors, retries, verify results and memory updates appear inline as one-line
  event cards (`✗`, `↻`, `✔`, `◆`). Nothing opens a dialog.
- Subagents get a live card with a spinner, model, remote host, their last
  action, elapsed time and cost. Enter or a click on its header opens the
  agent (its body is text you can select).
- Long sessions stay fast: only what is on screen is laid out, and finished
  entries are cached.

### Timestamps

Every message, assistant turn, tool card and event card shows a muted
timestamp on the right. `[ui] timestamps` chooses the style:

| value | shows |
|---|---|
| `time` (default) | `14:32:07` |
| `relative` | `now`, `12s ago`, `5m ago`, `2h ago`, `3d ago` |
| `off` | nothing |

Times are taken from the run's event log, so a live session and a replay of
it show the same times. (Your own messages use the time you sent them; they
will come from the log once it records user messages.)

### Scrolling

The transcript is anchored to the bottom, like a chat app: the newest
message sits just above the input, a short conversation sits at the bottom
of the pane with empty space above it, and older messages scroll away
upward. While you are at the bottom, new output appears there and the view
follows it.

PgUp/PgDn and the mouse wheel scroll. Scrolling up pauses following: new
output no longer moves the lines you are reading, and a
`↓ new messages · end` button appears on the bottom row of the transcript.
Any of these returns to the bottom and follows again:

- clicking the button, or focusing it (Alt+Up from the input) and
  pressing Enter;
- **End**, when the input is empty or its cursor is already at the end of
  the line (otherwise End first moves the cursor), or **Ctrl+End** at any
  time;
- scrolling back down to the end;
- the palette's `Jump to end`, or sending a message.

Resizing the terminal keeps a following view at the bottom and keeps a
scrolled-up view on the message you were reading. A scrolled-up view that
becomes tall enough to show everything follows again.

## Tool cards

Every tool call is a card. The header shows the status icon (`○` pending,
spinner while running, `✔` ok, `✗` error, `⏸` waiting for approval), the tool
name, a summary of the arguments, badges, and on the right the duration and
timestamp. The card background is tinted by its state. Nothing in a card
is cut off: when the summary and badges do not fit in the top border, the
icon and tool name stay there and the rest wraps onto rows inside the box;
long output, code and diff lines wrap inside the box too (continuation
rows leave the line-number gutter blank), and copy buttons that do not fit
in the bottom border get their own row above it.

| tool | header | body |
|---|---|---|
| `read` | path, `L10-60 · 51 lines`, `truncated` when more remains | numbered lines, highlighted |
| `write` | path, size, `new file` for new files | the content |
| `edit` | path, `+3 −1` | unified diff with line numbers, `+`/`-` colours and syntax colours |
| `bash` | `$ command`, `exit 101` / `timeout` / `killed` / `signal 9` | live output while running; `⋯ 212 lines truncated · full output saved as spill:…` when output was cut |
| `todo`, `memory`, `spawn_agent` | one compact line (task counts, memory change, subagent and task) | the details when expanded |
| MCP tools | `mcp <server> › <tool>` and the arguments sent to the server (also for calls through `mcp_tools`) | the server's output (`spill:…` pointer when it was cut) |

Bodies longer than `[ui] tool_preview_lines` (default 8) are collapsed with
`… N more lines · Enter to expand`. Click a card (its header, body or the
expand hint), or focus it with Alt+Up from the input (then Shift+Tab /
arrows) and press Enter, to expand
or collapse it; Esc returns to the input. On the body the click acts on the
release, so dragging over its output selects text instead (see
[tui.md](tui.md#copy-and-paste)). A failed call always shows the complete error message the model
received. Calls made in parallel in one turn are grouped under
`⧉ N parallel calls · 2 ok · 1 running`.

The agent map's live pane (Space or a click on a node) lists a node's MCP
calls the same way, with ✔/✗ and the duration, and node detail shows the
full cards. A server that changes its tool list mid-session adds a notice
to the chat (`MCP server github changed its tools ...`).

## Activity line

While the main agent works, a line above the input shows a spinner, the
current phase and how long it has been in it, the turn number and total turn
time, live token counts, the turn's cost and the interrupt key:

```
⠹ thinking 3.1s · turn 4 · 12.3s · ↑12.4k (cache 11.9k) ↓812 · $0.021 · ctrl+c to interrupt
⠼ running bash: cargo test parse 8.1s of 10m · cpu 6.3s (4 processes) · streaming 214 lines · turn 4 · …
⠧ retrying in 4s (429 rate limited, attempt 2/5) …
⏸ waiting for approval: git push origin main …
```

Phases: sending, waiting for first token, thinking, writing, running a tool
(name and summary), retrying (reason and countdown), waiting for approval,
verifying, compacting and syncing to a remote host.

What the line says when nothing visible is happening depends on what is
being waited on (`[ui] stall_after`, default 20 s, sets the threshold;
`0` turns detection off):

- **Waiting on the model.** If no data arrives from the provider stream for
  `stall_after`, the line turns orange and says so, with the silent time:
  `⚠ waiting on the model: no data for 41s · waiting for first token 41s · …`.
- **A tool running and alive.** A running tool is shown as working, never
  as stalled, with its elapsed time against its timeout (`bash_timeout`,
  or the call's own timeout capped at `bash_max_timeout`). For `bash`,
  harness samples the command's process group every 3 s (CPU time and live
  processes; on Linux from `/proc`, on macOS with one `ps` run, never per
  frame) and shows the group's CPU time, so a test suite that prints
  nothing for minutes still reads as busy:
  `⠹ running bash: cargo test --workspace 4m12s of 10m · cpu 14m03s (9 processes) · …`.
  When one response asks for several tools, the line follows the call
  that is actually running (bash reports when it starts), and a call
  that waited for approval resumes as the running tool once approved.
- **A tool running but idle.** When a sampled tool has printed nothing,
  used no CPU, had no runnable process and started or lost no process for
  `stall_after` (at least 6 s, two samples), a soft hint in the `warn`
  colour names it and the cancel key, without an alarm icon:
  `⠹ running bash: sleep 600 1m02s of 10m · cpu 4ms (2 processes) · bash quiet for 59s (no output, no CPU) · ctrl+c to cancel`.
  It does not claim the tool is hung: `sleep`, a lock wait or a slow
  network call is idle in the same way. Press the interrupt key to cancel
  it if it should not be idle.
- **Liveness unknown.** Tools that are not sampled (other tools, a remote
  host reached through the plain-shell fallback instead of the helper)
  fall back to elapsed time against the timeout, plus a neutral
  `no output for 45s` note after `stall_after`; they are never flagged.

Remote `bash` calls report the same liveness through the remote helper
(see [remote.md](remote.md)). Waiting for you never counts as a stall.

Spinners animate (~80 ms) only while something runs and is on screen; a
tick redraws only the spinner cells. `[ui] animations = false` shows a
static `◐` instead. With `[ui] terminal_title = true` the terminal title
shows the spinner and phase, e.g. `⠹ thinking · 12.3s — harness`
(`⚠ waiting on the model 41s` or `⠹ bash quiet 59s` in the cases above).

## Input

- Enter sends; Shift+Enter or Alt+Enter inserts a new line (see `[keys]`).
  Arrows, Home/End, Backspace and Delete edit. Long lines wrap; the input
  grows up to 8 rows.
- Up on the first line recalls earlier messages; Down on the last line goes
  forward again and finally restores what you were typing.
- Pasting is bracketed: pasted text is never sent by accident and arrives
  exactly, tabs and Unicode included (tabs show as spaces to the next
  multiple of four). A large paste
  (more than 8 lines or 1000 bytes) collapses to `[paste #1 · 120 lines]`.
  Backspace removes it as one unit; **Ctrl+E** (fixed) expands all
  placeholders in place, and clicking a placeholder (or focusing it and
  pressing Enter) expands that one. The full text is always what gets sent.
- Typing `/` at the start opens completion of slash commands with their
  arguments, description and scope. Matching is fuzzy, with the same
  matcher as the command palette: letters may have gaps (`/mdl` finds
  `/model`), namespaced commands such as `project:deploy` match on `deploy`
  as well as on the full name, and descriptions are searched too, so
  `/review` also finds a workflow whose description mentions review. Exact
  name matches come first, then names that start with what you typed, then
  other name matches by score, then description matches; equal scores stay
  in alphabetical order. Matched letters are highlighted (`highlight`
  token). Smart case: typing an upper-case letter makes the match case
  sensitive. Up/Down select, Tab or Enter complete, Esc closes, and rows
  are clickable. Enter on a complete command that takes no arguments sends
  it.
- After a command whose first argument has fixed choices (`/effort `,
  `/mode `, `/cost `, `/config `, ...), the same popup lists the choices
  that start with what you typed, each with the command line it completes
  to. Up/Down select, Tab or Enter complete, Esc closes, and rows are
  clickable. Enter with nothing typed yet sends the bare command (for
  `/effort`, its picker), and Enter on a fully typed choice sends it.

## Command palette

**Ctrl+P** opens one fuzzy-searchable list of everything you can do:
built-in commands, workflows, skills, agents, past runs, remotes and
settings (including the autonomy modes `ask`, `auto-edit`, `auto`, `yolo`). Each row
shows the title, description, where it comes from (kind and scope such as
`skill · project`) and its key binding, if it has one.

Type to filter, Up/Down to select, PgUp/PgDn to page, Enter to run. Entries
that need arguments (a workflow's input, an agent's task, a skill's
arguments) ask for them inline, one at a time; Esc goes back to the list and
Esc again closes the palette. Rows are clickable and a click outside closes
it. Opening and filtering stay within one frame even with 1000 entries.

Past runs are titled `Coder · fix the header parser · 14:32 Oct 23 · ✔`
(agent or workflow, first prompt or workflow inputs, start time, status)
with the outcome as the description; a short `#tag` is added after the
title only when two titles collide (`Coder · fix it #7a2b02 · 14:32 Oct 23 · ✔`).
In a narrow palette the title is cut, never the tag, time or status. With an
empty query past runs stay newest first. Agents are listed by their slash
command with their `display_name` and one-line description
(`/coder  Switch to Coder: Default coding agent (built in)`); `/agents` lists them the
same way.

Built-in entries: Toggle mouse, Show keys, Switch view, Interrupt,
Notifications, Jump to end, Quit, Timestamps: cycle, Thinking: cycle,
Mode: ask / auto-edit / auto / yolo (for this session; the status line
updates),
and the copy actions: Copy focused, Copy last message, Copy last code
block, Copy tool output (with or without line numbers), Copy diff and
Select text (see [Copy and paste](tui.md#copy-and-paste)).

## Approvals

When the policy needs your decision, the request appears inline above the
input, as a card per request:

```
╭ ⏸ approve bash  $ git push origin main ──────────────────── outward action ╮
│ rule: builtin · outward guard: git push*                                    │
│ [y] once  [s] session: git push *  [a] always  [n] deny  esc denies all     │
╰─────────────────────────────────────────────────────────────────────────────╯
```

- Bash requests show the parsed command that needs approval and the rule
  that matched (layer, origin, pattern and reason). Writes and edits show a
  diff preview (up to 12 lines).
- `y` approves once, `s` approves for the rest of the session using the
  suggested pattern, `a` approves always (you are asked separately before
  the rule is added to `[policy] bash.allow` in harness.ini), `n` denies.
- Several requests from one turn are answered together or one by one:
  Up/Down move between them, `Y` approves all open requests once, `N` denies
  them. Esc denies everything still open.
- Every button is clickable and reachable with Shift+Tab / arrows + Enter.
- While waiting, the tool card turns orange (`⏸`) and the activity line shows
  `⏸ waiting for approval: …`.

## Notifications

Background events (memory updates, recoverable errors such as a remote
disconnect, budget warnings, finished runs) pop up as toasts in the top
right. They never take focus, disappear after 6 seconds (at most 3 at a
time), and a click dismisses one. The palette entry **Notifications** opens
the full history (newest first, with timestamps); Up/Down move, Esc or
`[ close ]` closes it.

## Status line

The footer always shows, in theme colours: the autonomy mode first (the
last part ever left out; `yolo` in the error colour; Shift+Tab in the
input changes it), working directory and git branch, model, the reasoning
effort when one is set (`effort low`, or `no reasoning` on a model that
does not reason; see [`/effort`](commands.md#effort)), a context usage bar (green, orange above
60 %, red above 85 %), session tokens with the cache hit rate, session cost
(`$?` until known), the verify state (spinner, `✔` or `✗`), the number of
running agents and the remote hosts in use. When an MCP server has
failed, a red `⚠ mcp <name>` (`⚠ mcp <n> failed` for several) shows after
the model; clicking it, or Alt+Up to it and Enter, opens `/mcp`.
While background bash jobs run (`bash` with `run_in_background`), a
`⚙ <n> jobs` indicator (running colour) shows; clicking it, or Alt+Up
to it and Enter, opens `/jobs`: a panel listing every job of the session
with its state, elapsed time, command and log file, with buttons (click,
or Tab/arrows and Enter; Esc closes) to open a job's latest output (`output
j1`, with `refresh` and `stop` there too), stop a running job (`stop j1`,
its whole process group) or refresh the list. A job's end also reaches the
agent as a notice on its next request ([tools.md](tools.md#background-jobs-run_in_background)). It
updates from run events as they arrive. When the segments do not fit on one
row, the status line wraps onto more rows (up to three, and at most a sixth
of the screen) instead of cutting anything; long paths, branches and model
names wrap too. When even that is not enough, the least important parts
(remotes, path, tokens, verify, agents) are left out and a `+N more` button
ends the last row: click it, or Alt+Up to it and Enter, to open the full
status in a panel (directory, branch, model and mode, context, tokens,
cost, verify, goal, agents, jobs, remotes and every failed MCP server). The
palette entry **Show status** opens the same panel at any time.

An open session with nothing in flight, including a fresh one waiting for
its first prompt, shows as ready, not running: a run counts as working only
while a node runs, is queued or waits for approval, or while a workflow run
has not created its first node yet. The map header follows the same rule.
