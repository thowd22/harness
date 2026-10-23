# Run visibility: map, node detail, inspectors, history, goal bar, diff

This page covers the visibility views of the TUI (`crates/harness-tui`). They
are all folds over the run's event log (`harness_events::reducer::RunState`),
so a live run, a run opened from history and a replay look the same.
Everything visible can be clicked and operated from the keyboard
(Tab / arrows to focus, Enter to activate, Esc to back out, PgUp/PgDn and
the wheel to scroll).

## Agent map (`harness_tui::map`)

Tab toggles chat and map. The map draws workflow steps as a layered graph
(left to right, by `needs`) and hangs spawned subagents below their
parent as a tree:

```
 fix-bug  ◐ running  2m14s  $0.39  ↑24.8k ↓1.4k cache 96%  critical path 2m14s  ⏸ 1 waiting
╭──────────────────────╮     ╭──────────────────────╮     ╭──────────────────────╮
│✔ repro               │────►│✔ prove-fail          │────►│⏸ fix         ⧉gpu-box│
│implementer · sonnet  │     │verify                │     │implementer · sonnet  │
│41.0s $0.080          │     │6.0s verify ✗         │     │1m26s $0.31 ↻1        │
│↑12.4k ↓812 cache 96% │     │                      │     │↑12.4k ↓600 cache 97% │
│                      │     │                      │     │⏸ approval · Enter    │
╰──────────────────────╯     ╰──────────────────────╯     ╰─┬────────────────────╯
                                                            ├──►◐ researcher (haiku) 12s
                                                            └──►✔ researcher (haiku) 9.0s
```

- **Header**: run name and status, elapsed time, total cost, tokens with
  the cache hit rate, the critical path (the longest `needs` chain weighted
  by node duration) and how many nodes wait on you.
- **Nodes**: status icon (○ queued, spinner running, ⏸ waiting, ✔ ok,
  ✗ failed, ⊘ skipped/cancelled), name, agent and model, host badge,
  duration, cost, retries (`↻N`), last verify result, live tokens and cache
  hit rate. A running node's last line is a live preview of what it is
  writing (reasoning is dimmed and italic); `[ui] live_preview = false`
  turns previews off.
- **Blocked nodes** (an approval is open, or the node stopped on its
  budget) get an orange border; Enter on them jumps to the prompt.
- **Density**: when the full layout does not fit the viewport, or the run
  has more than 40 nodes, nodes switch to one-line compact form.
- **Keys**: arrows or `hjkl` move the selection to the nearest node in that
  direction and the view pans to keep it visible. Until you move it, the
  selection follows the active node; `f` resumes following. Enter opens node
  detail, Space toggles the live pane, Esc closes the live pane.
- **Mouse**: click a node to select it and open its live pane; click the
  selected node again to open its detail. The wheel pans.
- **Live pane**: under the map, streams the selected agent's reasoning,
  text and tool calls (with live bash output) token by token. `[x]` closes
  it. When a provider hides its reasoning the pane says
  "reasoning not exposed by provider" instead of showing nothing.

Layout is recomputed only when the run's topology changes (a node or edge
is added); status, cost and stream updates reuse the cached geometry, and
existing nodes keep their places when nodes are added.

## Node detail (`harness_tui::detail`)

GitHub-Actions-style log of one node. The sidebar lists its turns, tool
calls, verify runs and the hygiene report; activating an item scrolls the
log to it. The log has a collapsible group per model turn whose header
shows duration, tokens (in, cached, out, reasoning) and cost. A turn holds
its reasoning (expanded while it streams, then collapsed per
`[ui] show_thinking`), its text, the same tool cards as the chat and the
chat's metadata line. Failed tool calls, failed verify runs and the turns
that contain them are expanded automatically. Follow mode (`f`) tails a
running node.

Palette actions while detail is shown: **Search log**, **Copy entry**,
**Open diff**, **Open request inspector**, **Follow log**. The hygiene
section shows the change-ledger summary and the hygiene report with
`[review diff]` and `[remove N temp files]` buttons.

## Request inspector (`harness_tui::inspector`)

For any model call: the request payload exactly as recorded, the full
request rebuilt from the log (every earlier message and assistant turn of
the node, the system prompt, and the tool definitions, which the log
records in full whenever they change), the provider's wire body, the raw
response message and the streamed text and reasoning. In the TUI the wire
body is rendered by the session's own provider (`Provider::request_body`),
so it is exactly what was sent; elsewhere the Anthropic body is rebuilt
with the built-in model profiles. The **Layers**
tab breaks the prompt into tool list, system prompt and messages with an
estimated token count (bytes / 4), marks messages new in this call and the
cache breakpoints, and shows the reported input tokens. Everything is
redacted again before it is shown. When the provider recorded it, the
Request tab starts with the request body size, raw and on the wire, e.g.
`request body 279.5 KB raw · 46.2 KB on the wire (gzip, 83% smaller)`; the
export has it as `request_body`. `[export]` (or `e`) writes it all to
`request-<call_id>.json`. Tabs: click, or `1` `2` `3`.

## `/context` (`harness_tui::inspector::context`)

The initial context before the first user message: every system prompt
layer (with its YAML or `file:` source; the built-in identity layer is
listed as `identity layer`), every tool schema, the memory
index (with its `MEMORY.md` path) and the skill listing, each with an
estimated token count, plus the total and its share of the model's context
window. Rows expand to a preview of their text.

## Run history, replay and `harness runs show` (`harness_tui::history`)

The history lists runs newest first with status, workflow or agent, start
time, duration, cost and host. The palette filters it (**Filter runs**,
**Clear run filter**); Enter or a click opens the run in the map, `r` or
**Replay run** replays it.

Replay re-folds the events up to a cursor, and the map and node detail
show the run as it was at that point. The bar under the map has
`[◀◀ node] [◀ event] [event ▶] [node ▶▶]` buttons and a clickable
scrubber; keys are ←/→ (event), `[` `]` (previous/next node start or
finish), Home/End.

`harness runs show <id|prefix|last|#tag> [--json]` prints the same run as a
text tree, or the reducer state as JSON. It works on finished and live runs.

### Run and agent names

Runs and agents are shown by name, never by raw id. A run is titled after
its first prompt (or a workflow's inputs) and listed as
`Coder · fix the header parser · 14:32 Oct 23 · ✔`, with how it went
(`Succeeded in 2m14s · $0.12 · 3 files changed`) beside it. Agents show
their `display_name:` from their YAML (`Coder`, `Researcher`, `Reviewer`
for the built-ins; other agents default to their name with a capital first
letter). The palette, `/runs`, the history, the map header and nodes, node
detail, the goal bar, subagent cards and `harness runs show` all use the
same names. When two runs would read the same, each gets a short `#tag`
(the end of its id), which `harness runs show` also accepts.

## Goal bar (`harness_tui::goalbar`)

A right-hand pane with the goal, the task list (status icon, title, owning
agent, model, host badge) and done/total in the header. In-progress tasks
are highlighted and changes appear live. It only appears while there is at
least one task. Click a task, or focus it and press Enter, to jump to the
owning agent's node. On terminals too narrow for the pane next to 60
columns of main view, it collapses to a `☰ 2/5 <current task>` indicator
for the status line.

Tasks come from the `todo` tool (`add`, `update`, `list`; see
[subagents.md](subagents.md#todo-the-shared-task-list)), for example:

```json
{"action": "add", "title": "Reproduce with failing test"}
{"action": "update", "id": "t1", "status": "in_progress"}
```

`status` is one of `pending`, `in_progress`, `blocked`, `done`, `failed`,
`cancelled`. The owner of a task is the node that last claimed or created
it. Workflows can set the goal and tasks directly. The built-in coder is
asked to plan with `todo` for multi-step work and is reminded when it does
not (`[general] todo_nudge_after`, see
[subagents.md](subagents.md#planning-by-default)).

Configuration (`[ui]` in `harness.ini`): `goal_bar = auto | off` and
`goal_bar_width = 36`. The palette entry **Toggle goal bar** shows or hides
it for the session.

## Diff review (`/diff`, `harness_tui::diff`)

Changed files of the session or of one node, from the change ledger: kind,
`+N −M`, change sources and hygiene classifications inline
(`⚑ out of scope`, `⚑ temp/debug`, `⚑ whitespace-only`, `⚑ new untracked`),
and the selected file's diff with syntax colours. `[revert]` (or `r` on the
selected file) restores a file from its checkpoint.

## UI gallery (`harness ui-demo`, `harness_tui::demo`)

`harness_tui::demo::gallery::Gallery` renders every component from
scripted data: chat (cards, working indicator, timestamps, subagent
cards), map, node detail, goal bar, palette (with past runs), approvals,
diff, the request inspector and the run history. Click a tab or use ←/→; `q` quits.
`demo::gallery::FAKE_SCRIPT` is the fake-provider script that plays the
same session through the real agent loop. Every page has a styled
snapshot test, and an audit test fails if any page shows a raw UUID or
hash.

## Performance budget

`crates/harness-tui/tests/perf.rs` renders steady-state frames and fails
when the median frame exceeds the budget: 16 ms in release builds, scaled
by 4 in debug builds (as `cargo test` runs in CI); override with
`HARNESS_PERF_BUDGET_MS`. It covers the map at 200 nodes, node detail at
10k log entries, ten concurrent live streams with the goal bar, and
spinner frames. Spinners only tick while something runs and is visible, so
an idle UI requests no redraws. Measured release medians: map 0.9 ms,
detail 3.7 ms, ten streams + goal bar 0.4 ms.
