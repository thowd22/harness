---
id: doc-5
title: Agent Map and Run Visibility
type: specification
created_date: '2026-10-22 14:26'
updated_date: '2026-10-22 16:56'
---
# Agent Map & Run Visibility (UX spec)

Goal: GitHub-Actions-level visibility of every agent, subagent and workflow step, presented as a navigable map.

## Views (Tab cycles Chat ↔ Map; Enter drills in; Esc goes back)

### 1. Map
A live graph of the current (or selected) run.

```
 fix-bug  ● running  2m14s  $0.42                                      ⧉ gpu-box
 ┌──────────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
 │✔ repro       │───►│✔ prove-fail  │───►│◐ fix         │───►│○ proof       │──► …
 │ implementer  │    │ verify ✗→ok  │    │ sonnet  ⧉gpu │    │ bugfix-proof │
 │ 41s  $0.08   │    │ 6s           │    │ 1m02 $0.31   │    │              │
 └──────────────┘    └──────────────┘    └──────┬───────┘    └──────────────┘
                                                 ├─► ◐ researcher (haiku) 12s
                                                 └─► ✔ researcher (haiku) 9s
```

- Workflow steps laid out as a layered DAG (by `needs`); subagents hang off their parent as a tree.
- Node shows: status icon (○ queued, ◐ running, ⏸ waiting approval, ✔ ok, ✗ failed, ⊘ skipped/cancelled), name, agent/model, host badge, duration, cost; retries/loop iteration counts.
- Arrows / hjkl move selection; selection auto-follows the active node unless the user moved it.
- Large graphs switch to compact nodes (one line) automatically; viewport pans to keep selection visible.
- Nodes waiting for approval are highlighted and the approval prompt is reachable directly (Enter).

### 2. Node detail (GitHub Actions job log style)
Left: step list for the node (turns, tool calls, verify runs, hygiene report). Right: log.

```
 ▾ Turn 3   sonnet  in 12.4k (cache 11.9k) out 812   $0.021   4.2s
   ▸ thinking (collapsed)
     I'll update parse_header to handle …
   ▾ edit src/parse.rs                      ✔ 3ms
       @@ -40,7 +40,9 @@ …
   ▾ bash  cargo test parse                 ✗ exit 101  8.1s
       … 2 failed …
 ▸ Turn 4 …
```

- Collapsible groups per turn and per tool call; failures auto-expanded; follow mode (tail) while running.
- Each entry: duration, tokens (in/cached/out/reasoning), cost, retries.
- Palette actions: search log, copy entry, open request inspector, open diff, re-run from here (workflows).

### Live output and reasoning (real time)
Every agent's output text and reasoning stream live, token by token, in every view that shows that agent:

- **Map:** each running node shows a one-line live preview (latest output or reasoning line, dimmed for reasoning). Pressing Space on the selected node or clicking it opens a **live pane** under the map that streams the selected agent's output and reasoning while the map stays visible.
- **Node detail:** the current turn streams its reasoning block (expanded while it streams, then collapsed per `[ui] show_thinking`), its text, and its tool calls as they are produced. Follow mode tails the stream.
- **Parallel agents:** all streams are buffered per node; rendering is throttled to the frame budget so 10+ concurrent streams stay smooth.
- **Provider honesty:** reasoning is shown as the provider exposes it: Anthropic thinking (streamed), OpenAI reasoning summaries (requested with summaries enabled; raw reasoning is encrypted), OpenRouter reasoning deltas where the upstream model provides them. When reasoning is hidden or encrypted, the UI says so ("reasoning not exposed by provider") rather than showing nothing.

### Goal bar (right side)
A right-hand pane, similar to opencode's todo sidebar, showing the run's goal and its tasks. **It only appears when the current session or run has tasks defined**, and disappears when none exist.

```
 ┌ Goal ─────────────────────────────┐
 │ Fix header parsing bug (#142)     │
 ├ Tasks ──────────── 2/5 ───────────┤
 │ ✔ Reproduce with failing test     │
 │   implementer · sonnet            │
 │ ◐ Fix parse_header                │
 │   implementer · sonnet · ⧉gpu-box │
 │ ◐ Survey other callers            │
 │   researcher · haiku              │
 │ ○ Bug-fix proof                   │
 │ ○ Independent review              │
 └───────────────────────────────────┘
```

- Sources: tasks created by agents via the `todo` tool (shared by all agents in a run), and optional `goal`/`tasks` declared in workflow YAML.
- Each task shows status (○ pending, ◐ in progress, ⏸ blocked, ✔ done, ✗ failed, ⊘ cancelled), title, owning agent, model and host badge. In-progress tasks are highlighted; the header shows done/total.
- Clicking a task or pressing Enter on it jumps to the owning agent's node detail (or map node).
- Visible in both Chat and Map views. On narrow terminals it collapses to a one-line progress indicator in the status line; the palette can toggle it.

### Interaction rule: everything visible is clickable and keyboard-operable
Every interactive element on screen (map nodes, goal-bar tasks, log groups, palette entries, approval buttons, diff files, run-history rows, toasts) can be **clicked with the mouse** and **operated with the keyboard** (arrows/Tab to focus, Enter to activate, Esc to back out, wheel/PgUp/PgDn to scroll). This is implemented once in a hit-test and focus layer, so components only register regions and actions. Mouse capture can be turned off (`[ui] mouse = false`) to restore native terminal text selection.

### 3. Request inspector
Exact payload sent to the provider and raw response/stream for any model call, redacted; plus the prompt-layer breakdown with token counts. Primary tool for debugging harness/config fragility.

### 4. Run history
List of past runs (status, workflow/agent, start, duration, cost, host). Open any run in Map; replay it with a time scrubber (palette: "Replay run").

## Data model
Views are folds over the run's event log (`harness-events` reducer) so live, history and replay share one code path. The same reducer powers `harness runs show <id> --json`.

## Performance
Incremental reducer updates; layout recomputed only when the graph topology changes; render ≤16 ms at 200+ nodes.

## Implementation notes (harness-tui)
- Steps are nodes without a parent, children of a workflow container, or nodes in a `needs` edge; every other spawned node is a tree item under its nearest step. Workflow nodes with children are containers and are not drawn.
- Selection: click selects a node and opens its live pane, a second click (or Enter) opens node detail; Space toggles the live pane. Enter on a blocked node (approval or budget stop) jumps to the prompt.
- Compact nodes are used when the full layout does not fit the viewport or the run has more than 40 nodes.
- Goal-bar tasks come from `todo` tool calls carrying the full list (`{"goal"?, "todos": [{"id"?, "content"|"title", "status"}]}`); the owner of a task is the node that last changed it. The bar collapses to a status-line indicator below 60 + goal_bar_width columns.
- User docs: `docs/visibility.md`.
