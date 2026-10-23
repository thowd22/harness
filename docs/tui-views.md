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
~/CODE/harness  main │ sonnet · auto-edit │ ctx ▓▓▓░░░░░ 41% │ ↑182k ↓14k cache 93% │ $0.84 │ verify ✔ │ ◐ 3 agents │ ⧉ gpu-box
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
  action, elapsed time and cost. Enter or a click opens the agent.
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

PgUp/PgDn and the mouse wheel scroll. When you scroll back, new output no
longer moves the view; scrolling to the end (or the palette's `Jump to end`)
follows new output again.

## Tool cards

Every tool call is a card. The header shows the status icon (`○` pending,
spinner while running, `✔` ok, `✗` error, `⏸` waiting for approval), the tool
name, a summary of the arguments, badges, and on the right the duration and
timestamp. The card background is tinted by its state.

| tool | header | body |
|---|---|---|
| `read` | path, `L10-60 · 51 lines`, `truncated` when more remains | numbered lines, highlighted |
| `write` | path, size, `new file` for new files | the content |
| `edit` | path, `+3 −1` | unified diff with line numbers, `+`/`-` colours and syntax colours |
| `bash` | `$ command`, `exit 101` / `timeout` / `killed` / `signal 9` | live output while running; `⋯ 212 lines truncated · full output saved as spill:…` when output was cut |
| `todo`, `memory`, `spawn_agent` | one compact line (task counts, memory change, subagent and task) | the details when expanded |

Bodies longer than `[ui] tool_preview_lines` (default 8) are collapsed with
`… N more lines · Enter to expand`. Click a card, or focus it with
Shift+Tab / arrows and press Enter, to expand or collapse it; Esc returns to
the input. A failed call always shows the complete error message the model
received. Calls made in parallel in one turn are grouped under
`⧉ N parallel calls · 2 ok · 1 running`.

## Activity line

While the main agent works, a line above the input shows a spinner, the
current phase and how long it has been in it, the turn number and total turn
time, live token counts, the turn's cost and the interrupt key:

```
⠹ thinking 3.1s · turn 4 · 12.3s · ↑12.4k (cache 11.9k) ↓812 · $0.021 · ctrl+c to interrupt
⠼ running bash: cargo test parse 8.1s · streaming 214 lines · turn 4 · …
⠧ retrying in 4s (429 rate limited, attempt 2/5) …
⏸ waiting for approval: git push origin main …
```

Phases: sending, waiting for first token, thinking, writing, running a tool
(name and summary), retrying (reason and countdown), waiting for approval,
verifying, compacting and syncing to a remote host.

If no data arrives from the provider, or no output from a running tool, for
`[ui] stall_after` (default 20 s), the line turns orange:
`⚠ no data for 21s (provider stream)`. Waiting for you never counts as a
stall.

Spinners animate (~80 ms) only while something runs and is on screen; a
tick redraws only the spinner cells. `[ui] animations = false` shows a
static `◐` instead. With `[ui] terminal_title = true` the terminal title
shows the spinner and phase, e.g. `⠹ thinking · 12.3s — harness`.

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

## Command palette

**Ctrl+P** opens one fuzzy-searchable list of everything you can do:
built-in commands, workflows, skills, agents, past runs, remotes and
settings (including the autonomy modes `ask`, `auto-edit`, `auto`). Each row
shows the title, description, where it comes from (kind and scope such as
`skill · project`) and its key binding, if it has one.

Type to filter, Up/Down to select, PgUp/PgDn to page, Enter to run. Entries
that need arguments (a workflow's input, an agent's task, a skill's
arguments) ask for them inline, one at a time; Esc goes back to the list and
Esc again closes the palette. Rows are clickable and a click outside closes
it. Opening and filtering stay within one frame even with 1000 entries.

Built-in entries: Toggle mouse, Show keys, Switch view, Interrupt,
Notifications, Jump to end, Quit, Timestamps: cycle, Thinking: cycle,
Mode: ask / auto-edit / auto (for this session; the status line updates),
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

The footer always shows, in theme colours: working directory and git
branch, model and autonomy mode, a context usage bar (green, orange above
60 %, red above 85 %), session tokens with the cache hit rate, session cost
(`$?` until known), the verify state (spinner, `✔` or `✗`), the number of
running agents and the remote hosts in use. It updates from run events as
they arrive. On narrow terminals the least important parts (remotes, path,
tokens, verify, agents) are dropped first.
